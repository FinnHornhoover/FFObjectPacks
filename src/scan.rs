use crate::{
    appearance,
    cache::Cache,
    progress::Progress,
    runtime, shader, terrain,
    unity::{
        asset::{Asset, Object},
        bundle,
    },
    value::{Fields, array, content, map, packed, serial, write_json, write_pack},
};
use anyhow::{Context, Result};
use indexmap::IndexMap;
use rmpv::Value;
use serde::Serialize;
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::Path,
};

#[derive(Clone, Serialize)]
pub struct Summary {
    pub name: String,
    pub source: String,
    pub scene: bool,
    pub version: String,
    pub format: u32,
}

#[derive(Clone)]
pub struct Node {
    pub kind: i32,
    pub asset: usize,
    pub data: Value,
}

pub struct Scan {
    pub assets: Vec<Summary>,
    pub nodes: Vec<Option<Node>>,
    pub names: Vec<String>,
    pub keys: Vec<(u32, i64)>,
    name_ids: HashMap<String, u32>,
    key_ids: HashMap<(u32, i64), u32>,
    pub scenes: Vec<(usize, Vec<u32>)>,
    pub terrains: Vec<u32>,
    pub clips: Vec<u32>,
    pub rigs: Vec<u32>,
    pub appearances: Vec<u32>,
    pub runtime_palettes: Vec<Value>,
    pub runtime_errors: Vec<Value>,
    pub prefabs: Vec<(String, Option<u32>)>,
    pub paths: IndexMap<String, u32>,
    pub errors: Vec<Value>,
    pub counts: IndexMap<String, u64>,
}

impl Scan {
    pub fn new() -> Self {
        Self {
            assets: Vec::new(),
            nodes: Vec::new(),
            names: Vec::new(),
            keys: Vec::new(),
            name_ids: HashMap::new(),
            key_ids: HashMap::new(),
            scenes: Vec::new(),
            terrains: Vec::new(),
            clips: Vec::new(),
            rigs: Vec::new(),
            appearances: Vec::new(),
            runtime_palettes: Vec::new(),
            runtime_errors: Vec::new(),
            prefabs: Vec::new(),
            paths: IndexMap::new(),
            errors: Vec::new(),
            counts: IndexMap::new(),
        }
    }

    pub fn node(&self, id: u32) -> Option<&Node> {
        self.nodes.get(id as usize).and_then(Option::as_ref)
    }

    pub fn oid(&mut self, name: &str, path: i64) -> u32 {
        let name = name.to_ascii_lowercase();
        let next = self.names.len() as u32;
        let asset = *self.name_ids.entry(name.clone()).or_insert_with(|| {
            self.names.push(name);
            next
        });

        let next = self.keys.len() as u32;
        *self.key_ids.entry((asset, path)).or_insert_with(|| {
            self.keys.push((asset, path));
            next
        })
    }

    pub fn source_key(&self, id: u32) -> Value {
        let (asset, path) = self.keys[id as usize];
        array::<Value>([self.names[asset as usize].clone().into(), path.into()])
    }

    fn pointer(&mut self, asset: &Asset, file: i64, path: i64) -> Option<u32> {
        if path == 0 {
            return None;
        }

        let index = if file < 0 {
            file + asset.refs.len() as i64
        } else {
            file
        };
        let name = if index >= 0 {
            asset.refs.get(index as usize)
        } else {
            None
        };

        if let Some(name) = name {
            Some(self.oid(name, path))
        } else {
            self.errors.push(map([
                ("kind", "bad-reference".into()),
                ("asset", asset.name.clone().into()),
                ("file_id", file.into()),
                ("path_id", path.into()),
            ]));
            Some(self.oid(&format!("unresolved:{}:{file}", asset.name), path))
        }
    }

    fn ptr(&mut self, asset: &Asset, value: &Value) -> Value {
        if let Value::Ext(-1, bytes) = value {
            let file = i64::from_le_bytes(bytes[..8].try_into().unwrap());
            let path = i64::from_le_bytes(bytes[8..].try_into().unwrap());
            self.pointer(asset, file, path)
                .map(Value::from)
                .unwrap_or(Value::Nil)
        } else {
            Value::Nil
        }
    }

    fn convert(&mut self, asset: &Asset, value: &mut Value) {
        match value {
            Value::Ext(-1, _) => {
                let id = self.ptr(asset, value);
                *value = map([("r", id)]);
            }
            Value::Map(m) => {
                for (_, v) in m {
                    self.convert(asset, v)
                }
            }
            Value::Array(a) => {
                for v in a {
                    self.convert(asset, v)
                }
            }
            _ => {}
        }
    }

    fn describe(
        &mut self,
        asset: &Asset,
        obj: &Object,
        mut raw: Value,
        rig: Option<&str>,
        output: &Path,
    ) -> Result<Value> {
        let tid = obj.kind;
        if tid == 74 {
            self.convert(asset, &mut raw);
            let mut single = raw.clone();
            float32(&mut single);
            let file = content(output, "animations/clips", &packed(&single)?, ".msgpack")?;

            let mut curves = Vec::new();
            let mut targets = BTreeSet::new();
            for (name, tracks) in raw.as_map().into_iter().flatten() {
                if !name.text().ends_with("Curves") || tracks.array().is_empty() {
                    continue;
                }
                curves.push(name.clone());
                for track in tracks.array() {
                    let path = if !track["path"].is_nil() {
                        &track["path"]
                    } else {
                        &track["m_Path"]
                    };
                    if let Some(path) = path.as_str() {
                        let next = self.paths.len() as u32;
                        targets.insert(*self.paths.entry(path.to_owned()).or_insert(next));
                    }
                }
            }

            return Ok(map([
                ("name", default(&raw, "m_Name", "".into())),
                ("file", file.into()),
                ("sample_rate", raw["m_SampleRate"].clone()),
                ("curve_types", array(curves)),
                ("paths", array(targets)),
            ]));
        }

        if let Some(rig) = rig {
            self.convert(asset, &mut raw);
            let mut result = map([("script_type", rig.into())]);
            result
                .as_map_mut()
                .unwrap()
                .extend(raw.as_map().unwrap().clone());
            return Ok(result);
        }

        Ok(match tid {
            43 => map([("name", default(&raw, "m_Name", "".into()))]),
            48 => {
                self.convert(asset, &mut raw);
                shader::describe(&raw, output)?
            }
            27 | 28 | 89 => {
                self.convert(asset, &mut raw);
                map([
                    ("name", default(&raw, "m_Name", "".into())),
                    ("width", raw["m_Width"].clone()),
                    ("height", raw["m_Height"].clone()),
                    ("format", raw["m_TextureFormat"].clone()),
                    ("settings", default(&raw, "m_TextureSettings", map([]))),
                ])
            }
            1 => {
                let mut components = Vec::new();
                for c in raw["m_Component"].array() {
                    let tid = c[0].integer() as i32;
                    if component(tid) {
                        components.push(array::<Value>([tid.into(), self.ptr(asset, &c[1])]));
                    }
                }
                map([
                    ("name", default(&raw, "m_Name", "".into())),
                    ("active", default(&raw, "m_IsActive", 1.into())),
                    ("layer", default(&raw, "m_Layer", 0.into())),
                    ("components", array(components)),
                ])
            }
            4 => {
                let go = self.ptr(asset, &raw["m_GameObject"]);
                let parent = self.ptr(asset, &raw["m_Father"]);
                map([
                    ("go", go),
                    ("parent", parent),
                    ("p", vector(&raw["m_LocalPosition"], "xyz")),
                    ("q", vector(&raw["m_LocalRotation"], "xyzw")),
                    ("s", vector(&raw["m_LocalScale"], "xyz")),
                ])
            }
            33 => {
                let go = self.ptr(asset, &raw["m_GameObject"]);
                let mesh = self.ptr(asset, &raw["m_Mesh"]);
                map([("go", go), ("mesh", mesh)])
            }
            156 => {
                let encoded = terrain::heights(&raw["m_Heightmap"])?;
                let file = content(output, "areas/terrain/heights", &encoded, ".bin")?;
                let mut height = raw["m_Heightmap"].clone();
                height.set(
                    "m_Heights",
                    map([
                        ("file", file.into()),
                        ("format", std::str::from_utf8(&encoded[..4])?.into()),
                    ]),
                );
                raw.set("m_Heightmap", height);

                let encoded = terrain::details(&raw["m_DetailDatabase"]["m_Patches"])?;
                let file = content(output, "areas/terrain/details", &encoded, ".msgpack")?;
                let mut details = raw["m_DetailDatabase"].clone();
                details.set("m_Patches", map([("file", file.into())]));
                raw.set("m_DetailDatabase", details);

                self.convert(asset, &mut raw);
                raw
            }
            _ => {
                self.convert(asset, &mut raw);
                raw
            }
        })
    }

    pub fn run(cache: &Cache, output: &Path, work: Option<&Path>) -> Result<Self> {
        let mut result = Self::new();
        let mut progress =
            Progress::for_path(&cache.root, "prepare/scan", cache.files.len(), "files");
        for (index, path) in cache.files.iter().enumerate() {
            let filename = path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase();
            if filename.contains("music") || filename.contains("fusound") {
                continue;
            }

            let source = path
                .strip_prefix(&cache.root)?
                .to_string_lossy()
                .replace('\\', "/");
            let Some(members) = bundle::read(path)? else {
                result.errors.push(map([
                    ("kind", "non-bundle".into()),
                    ("file", source.into()),
                ]));
                continue;
            };

            for member in members {
                if member.name.to_ascii_lowercase().ends_with(".dll") {
                    let normalized = member.name.to_ascii_lowercase().replace([' ', '-'], "");
                    if normalized.ends_with("assemblycsharp.dll") {
                        match runtime::palettes(&member.data, &member.name, &source) {
                            Ok(data) => result.runtime_palettes.extend(data),
                            Err(error) => result.runtime_errors.push(map([
                                ("file", source.clone().into()),
                                ("assembly", member.name.into()),
                                ("error", error.to_string().into()),
                            ])),
                        }
                    }
                    continue;
                }
                let asset = Asset::read(member).with_context(|| format!("Cannot read {source}"))?;
                let name = asset.name.to_ascii_lowercase();
                let scene = name.starts_with("buildplayer-") && !name.ends_with(".sharedassets");
                let aid = result.assets.len();
                result.assets.push(Summary {
                    name: name.clone(),
                    source: source.clone(),
                    scene,
                    version: asset.version.clone(),
                    format: asset.format,
                });

                let mut roots = Vec::new();
                for (object_index, obj) in asset.objects.values().enumerate() {
                    if object_index.is_multiple_of(8192) {
                        progress.update(
                            index,
                            format!(
                                "assets={} clips={} rigs={} errors={}",
                                result.assets.len(),
                                result.clips.len(),
                                result.rigs.len(),
                                result.errors.len()
                            ),
                        );
                    }

                    let tid = obj.kind;
                    let schema = asset.schema(obj, &cache.fallback);
                    let rig = if tid < 0 {
                        schema.and_then(|tree| {
                            let names: HashSet<_> =
                                tree.children.iter().map(|c| c.name.as_str()).collect();
                            if names.contains("actorBones") {
                                Some("ActorSkinCombiner")
                            } else if names.contains("transformIndicesM")
                                && names.contains("transformIndicesF")
                            {
                                Some("ActorWearIndexTable")
                            } else if names.contains("skins") {
                                Some("ActorWearSkinTable")
                            } else {
                                None
                            }
                        })
                    } else {
                        None
                    };

                    let appearance = (tid < 0)
                        .then(|| schema.and_then(appearance::classify))
                        .flatten();
                    let supported = visual(tid) || rig.is_some() || appearance.is_some();
                    if !supported && tid != 142 && !(scene && tid < 0) {
                        continue;
                    }

                    let mut id = None;
                    if tid == 74 {
                        let oid = result.oid(&name, obj.path);
                        result.clips.push(oid);
                        id = Some(oid);
                    }
                    if rig.is_some() {
                        let oid = result.oid(&name, obj.path);
                        result.rigs.push(oid);
                        id = Some(oid);
                    }
                    if supported {
                        id = Some(result.oid(&name, obj.path));
                    }
                    if appearance.is_some() {
                        result.appearances.push(id.unwrap());
                    }

                    let fields = match tid {
                        43 => Some(HashSet::from(["m_Name"])),
                        27 | 28 | 89 => Some(HashSet::from([
                            "m_Name",
                            "m_Width",
                            "m_Height",
                            "m_TextureFormat",
                            "m_TextureSettings",
                        ])),
                        _ => None,
                    };

                    let decoded = (|| -> Result<Option<Value>> {
                        let mut raw = asset.decode(obj, &cache.fallback, fields.as_ref())?;
                        if let Some(kind) = appearance {
                            raw = raw.without(&["m_GameObject", "m_Script", "m_ExtensionPtr"]);
                            raw.set("appearance_type", kind.into());
                        }
                        if tid == 142 {
                            for entry in raw["m_Container"].array() {
                                let target = result.ptr(&asset, &entry[1]["asset"]).id();
                                result.prefabs.push((entry[0].text().to_owned(), target));
                            }
                            return Ok(None);
                        }

                        if !supported {
                            if !raw
                                .as_map()
                                .into_iter()
                                .flatten()
                                .any(|(k, _)| k.text().to_ascii_lowercase().contains("terrain"))
                            {
                                return Ok(None);
                            }
                            id = Some(result.oid(&name, obj.path));
                        }
                        Ok(Some(result.describe(&asset, obj, raw, rig, output)?))
                    })();

                    match decoded {
                        Ok(Some(data)) => {
                            let id = id.unwrap();
                            if result.nodes.len() <= id as usize {
                                result.nodes.resize_with(id as usize + 1, || None);
                            }
                            result.nodes[id as usize] = Some(Node {
                                kind: tid,
                                asset: aid,
                                data,
                            });
                            if supported {
                                *result.counts.entry(tid.to_string()).or_default() += 1;
                                if tid == 156 {
                                    result.terrains.push(id);
                                }
                                if scene && [23, 26, 104, 108, 111, 137, 154, 157].contains(&tid) {
                                    roots.push(id);
                                }
                            } else {
                                roots.push(id);
                            }
                        }
                        Ok(None) => {}
                        Err(error) if !supported && tid != 142 => {
                            *result
                                .counts
                                .entry("unsupported_scene_scripts".into())
                                .or_default() += 1;
                            let _ = error;
                        }
                        Err(error) => {
                            if tid == 142 || tid == 74 || rig.is_some() || appearance.is_some() {
                                return Err(error).with_context(|| {
                                    format!(
                                        "Cannot extract {source} / {} / {} (type {tid})",
                                        asset.name, obj.path
                                    )
                                });
                            }
                            result.errors.push(map([
                                ("kind", "object".into()),
                                ("file", source.clone().into()),
                                ("asset", name.clone().into()),
                                ("path_id", obj.path.into()),
                                ("type", tid.into()),
                                ("error", error.to_string().into()),
                            ]));
                        }
                    }
                }

                if !roots.is_empty() {
                    result.scenes.push((aid, roots));
                }
            }

            progress.update(
                index + 1,
                format!(
                    "assets={} clips={} rigs={} errors={}",
                    result.assets.len(),
                    result.clips.len(),
                    result.rigs.len(),
                    result.errors.len()
                ),
            );
        }

        progress.finish();
        result.key_ids = HashMap::new();
        result.name_ids = HashMap::new();

        if let Some(work) = work {
            result.snapshot(work)?;
        }
        Ok(result)
    }

    pub fn metadata(&self) -> Value {
        map([
            ("assets", serial(&self.assets)),
            (
                "keys",
                array((0..self.keys.len()).map(|id| self.source_key(id as u32))),
            ),
            (
                "scene_roots",
                array(self.scenes.iter().map(|(asset, roots)| {
                    map([
                        ("asset", (*asset as u64).into()),
                        ("roots", array(roots.iter().copied())),
                    ])
                })),
            ),
            ("terrain_roots", array(self.terrains.iter().copied())),
            ("clip_roots", array(self.clips.iter().copied())),
            ("rig_roots", array(self.rigs.iter().copied())),
            ("appearance_roots", array(self.appearances.iter().copied())),
            ("runtime_palettes", array(self.runtime_palettes.clone())),
            ("runtime_errors", array(self.runtime_errors.clone())),
            ("animation_paths", array(self.paths.keys().cloned())),
            (
                "prefab_roots",
                array(self.prefabs.iter().map(|(path, id)| {
                    array::<Value>([
                        path.clone().into(),
                        id.map(Value::from).unwrap_or(Value::Nil),
                    ])
                })),
            ),
            ("errors", array(self.errors.clone())),
            ("counts", serial(&self.counts)),
        ])
    }

    fn snapshot(&self, work: &Path) -> Result<()> {
        use std::io::Write;

        std::fs::create_dir_all(work)?;
        let mut file = std::io::BufWriter::new(std::fs::File::create(work.join("nodes.msgpack"))?);
        for (id, node) in self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(id, n)| n.as_ref().map(|n| (id, n)))
        {
            file.write_all(&packed(&array::<Value>([
                (id as u32).into(),
                node.kind.into(),
                (node.asset as u64).into(),
                node.data.clone(),
            ]))?)?;
        }
        file.flush()?;

        let metadata = self.metadata();
        write_pack(&work.join("scan.msgpack"), &metadata)?;
        write_json(
            &work.join("scan-summary.json"),
            &metadata.without(&[
                "keys",
                "prefab_roots",
                "scene_roots",
                "animation_paths",
                "clip_roots",
                "rig_roots",
            ]),
        )
    }
}

fn float32(value: &mut Value) {
    match value {
        Value::F64(f) => *value = Value::F32(*f as f32),
        Value::Map(m) => {
            for (_, v) in m {
                float32(v)
            }
        }
        Value::Array(a) => {
            for v in a {
                float32(v)
            }
        }
        _ => {}
    }
}

fn default(raw: &Value, key: &str, fallback: Value) -> Value {
    if raw[key].is_nil() {
        fallback
    } else {
        raw[key].clone()
    }
}

fn vector(raw: &Value, axes: &str) -> Value {
    array(axes.chars().map(|c| raw[c.to_string().as_str()].clone()))
}

fn component(tid: i32) -> bool {
    tid < 0 || [4, 23, 26, 33, 108, 111, 137, 154].contains(&tid)
}

fn visual(tid: i32) -> bool {
    [
        1, 4, 21, 23, 26, 27, 28, 33, 43, 48, 74, 89, 104, 108, 111, 137, 154, 156, 157,
    ]
    .contains(&tid)
}
