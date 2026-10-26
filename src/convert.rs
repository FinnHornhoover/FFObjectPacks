use crate::{
    cache::Cache,
    geometry,
    plan::Selection,
    progress::Progress,
    scan::Scan,
    texture,
    value::{Fields, content, map, packed, unpack, write_pack},
};
use anyhow::{Context, Result};
use rayon::prelude::*;
use rmpv::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

const GEOMETRY_FIELDS: &[&str] = &[
    "m_MeshCompression",
    "m_CompressedMesh",
    "m_Vertices",
    "m_Normals",
    "m_UV",
    "m_UV1",
    "m_UV2",
    "m_UV3",
    "m_Colors",
    "m_Tangents",
    "m_SubMeshes",
    "m_Use16BitIndices",
    "m_IndexBuffer",
    "m_Skin",
    "m_BindPose",
];
type Cached = Arc<OnceLock<std::result::Result<Value, String>>>;
pub struct Store {
    root: PathBuf,
    entries: Mutex<HashMap<String, Cached>>,
}

impl Store {
    pub fn new(root: &Path) -> Result<Self> {
        let root = root.join(crate::FORMAT_VERSION.to_string());
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            entries: Mutex::new(HashMap::new()),
        })
    }

    fn get(&self, key: &str, convert: impl FnOnce() -> Result<Value>) -> Result<(Value, bool)> {
        let entry = self
            .entries
            .lock()
            .unwrap()
            .entry(key.to_owned())
            .or_default()
            .clone();

        let mut reused = true;
        let result = entry.get_or_init(|| {
            let path = self.root.join("keys").join(format!("{key}.msgpack"));
            if path.exists() {
                return unpack(&fs::read(path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string());
            }

            reused = false;
            (|| {
                let value = convert()?;
                write_pack(&path, &value)?;
                Ok(value)
            })()
            .map_err(|e: anyhow::Error| format!("{e:#}"))
        });

        result
            .clone()
            .map(|value| (value, reused))
            .map_err(anyhow::Error::msg)
    }

    fn materialize(&self, result: &Value, output: &Path) -> Result<()> {
        for field in ["file", "skin_data"] {
            if let Some(relative) = result[field].as_str() {
                let source = self.root.join(relative);
                let target = output.join(relative);
                if target.exists() {
                    continue;
                }

                fs::create_dir_all(target.parent().unwrap())?;
                if let Err(error) = fs::hard_link(&source, &target)
                    && !target.exists()
                {
                    fs::copy(source, &target)
                        .with_context(|| format!("Cannot materialize {relative}: {error}"))?;
                }
            }
        }
        Ok(())
    }

    fn mesh(&self, raw: Value, output: &Path) -> Result<(Value, bool)> {
        let normalized = geometry_key(raw);
        let key = format!("mesh-{}", hex::encode(Sha256::digest(packed(&normalized)?)));

        let (result, reused) = self.get(&key, || {
            let mesh = geometry::convert(&normalized)?;
            let mut result = mesh.info;
            result.set(
                "file",
                content(&self.root, "models/geometry", &mesh.glb, ".glb")?.into(),
            );
            if let Some(skin) = mesh.skin {
                result.set(
                    "skin_data",
                    content(&self.root, "models/skins", &skin, ".msgpack")?.into(),
                );
            }
            Ok(result)
        })?;

        self.materialize(&result, output)?;
        Ok((result, reused))
    }

    fn texture(&self, raw: &Value, output: &Path) -> Result<(Value, bool)> {
        let image = texture::decode(raw)?;

        let mut digest = Sha256::new();
        digest.update(image.width().to_le_bytes());
        digest.update(image.height().to_le_bytes());
        digest.update(image.as_raw());
        let key = format!("texture-{}", hex::encode(digest.finalize()));

        let (result, reused) = self.get(&key, || {
            Ok(map([
                (
                    "file",
                    content(&self.root, "textures", &texture::encode(&image)?, ".webp")?.into(),
                ),
                (
                    "dimensions",
                    crate::value::array([image.width(), image.height()]),
                ),
                ("rgba_exact", true.into()),
            ]))
        })?;

        self.materialize(&result, output)?;
        Ok((result, reused))
    }
}

fn geometry_key(mut raw: Value) -> Value {
    let compressed = raw["m_MeshCompression"].truth();
    raw.set("m_MeshCompression", compressed.into());

    let submeshes = crate::value::array(raw["m_SubMeshes"].array().iter().map(|submesh| {
        map([
            ("firstByte", submesh["firstByte"].clone()),
            ("indexCount", submesh["indexCount"].clone()),
            ("isTriStrip", submesh["isTriStrip"].truth().into()),
        ])
    }));
    raw.set("m_SubMeshes", submeshes);

    let decoded = [
        "m_Vertices",
        "m_UV",
        "m_Normals",
        "m_Triangles",
        "m_NormalSigns",
    ];
    let mut channels = map([]);
    for (name, value) in raw["m_CompressedMesh"].as_map().into_iter().flatten() {
        let count = value["m_NumItems"].integer() as usize;
        if count == 0 || (!compressed && decoded.contains(&name.text())) {
            continue;
        }

        let channel = if decoded.contains(&name.text()) {
            let bits = value["m_BitSize"].integer() as usize;
            let mut bytes = value["m_Data"].binary().to_vec();
            bytes.truncate((count * bits).div_ceil(8));
            if !(count * bits).is_multiple_of(8)
                && let Some(last) = bytes.last_mut()
            {
                *last &= ((1u16 << (count * bits % 8)) - 1) as u8;
            }

            let mut channel = map([
                ("m_NumItems", value["m_NumItems"].clone()),
                ("m_BitSize", value["m_BitSize"].clone()),
                ("m_Data", Value::Binary(bytes)),
            ]);
            if ["m_Vertices", "m_UV", "m_Normals"].contains(&name.text()) {
                channel.set(
                    "m_Range",
                    if bits == 0 {
                        0.into()
                    } else {
                        value["m_Range"].clone()
                    },
                );
                channel.set("m_Start", value["m_Start"].clone());
            }
            channel
        } else {
            value.clone()
        };
        channels.set(name.text(), channel);
    }

    if compressed && channels["m_Vertices"].is_nil() {
        channels.set("m_Vertices", map([]));
    }
    raw.set("m_CompressedMesh", channels);
    if compressed {
        raw = raw.without(&[
            "m_Vertices",
            "m_Normals",
            "m_UV",
            "m_UV1",
            "m_UV2",
            "m_UV3",
            "m_IndexBuffer",
            "m_Use16BitIndices",
        ]);
    }

    raw.as_map_mut()
        .unwrap()
        .sort_by(|(a, _), (b, _)| a.text().cmp(b.text()));
    raw
}

pub fn export(
    cache: &Cache,
    scan: &Scan,
    selection: &[Selection],
    output: &Path,
    store: &Store,
) -> Result<BTreeMap<u32, Value>> {
    let mut groups: BTreeMap<String, Vec<&Selection>> = BTreeMap::new();
    for record in selection {
        groups
            .entry(scan.assets[record.asset].source.clone())
            .or_default()
            .push(record);
    }

    let done = AtomicUsize::new(0);
    let hits = AtomicUsize::new(0);
    let progress = Mutex::new(Progress::for_path(
        output,
        "finish/convert",
        selection.len(),
        "assets",
    ));
    let fields = HashSet::from_iter(GEOMETRY_FIELDS.iter().copied());

    let results: Result<Vec<_>> = groups
        .par_iter()
        .map(|(source, records)| {
            let mut converted = BTreeMap::new();
            let assets = cache
                .assets(&cache.root.join(source))?
                .context("Selected bundle is not a Unity bundle")?;

            for asset in assets {
                let wanted = records
                    .iter()
                    .filter(|r| scan.assets[r.asset].name == asset.name.to_ascii_lowercase());
                for record in wanted {
                    let obj = asset.objects.get(&record.path).with_context(|| {
                        format!("Missing selected object {} / {}", asset.name, record.path)
                    })?;
                    let raw = asset.decode(
                        obj,
                        &cache.fallback,
                        if record.kind == 43 {
                            Some(&fields)
                        } else {
                            None
                        },
                    )?;

                    let (result, reused) = if record.kind == 43 {
                        store.mesh(raw, output)
                    } else {
                        store.texture(&raw, output)
                    }
                    .with_context(|| {
                        format!(
                            "Cannot convert {source} / {} / {} (type {})",
                            asset.name, record.path, record.kind
                        )
                    })?;

                    converted.insert(record.object, result);
                    if reused {
                        hits.fetch_add(1, Ordering::Relaxed);
                    }
                    let count = done.fetch_add(1, Ordering::Relaxed) + 1;
                    progress
                        .lock()
                        .unwrap()
                        .update(count, format!("reused={}", hits.load(Ordering::Relaxed)));
                }
            }

            Ok(converted)
        })
        .collect();

    let mut combined = BTreeMap::new();
    for result in results? {
        combined.extend(result);
    }

    anyhow::ensure!(
        combined.len() == selection.len(),
        "Selected assets were not all converted"
    );
    progress.lock().unwrap().finish();
    Ok(combined)
}
