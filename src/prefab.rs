use crate::{
    gltf::{self, Document, curves::Clip, skin},
    plan::Plan,
    progress::Progress,
    scan::{Node, Scan},
    value::{Fields, map, rawref, serial, unpack, write_json},
};
use anyhow::{Context, Result, ensure};
use glam::DMat4;
use rmpv::Value;
use serde_json::{Value as Json, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::Path,
    sync::Arc,
};

mod clips;
mod scene;

struct Assembly {
    writer: Document,
    nodes: HashMap<u32, usize>,
    renderers: BTreeMap<u32, Vec<usize>>,
    owners: HashMap<usize, Vec<u32>>,
    animations: Vec<(u32, usize)>,
    warnings: Vec<String>,
    rig_count: usize,
}

pub struct Index {
    nodes: HashMap<u32, Node>,
    transforms: HashMap<u32, u32>,
    paths: Vec<String>,
}

impl Index {
    pub fn capture(scan: &Scan, plan: &Plan) -> Self {
        let mut transforms: HashMap<_, _> = scan
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(id, node)| {
                let node = node.as_ref()?;
                (node.kind == 4)
                    .then(|| node.data["go"].id().map(|go| (go, id as u32)))
                    .flatten()
            })
            .collect();
        let mut todo: Vec<_> = plan
            .catalogs
            .iter()
            .filter_map(|entry| transforms.get(&entry.object).map(|id| (*id, true)))
            .collect();
        let mut nodes = HashMap::new();
        let mut expanded = HashSet::new();
        while let Some((id, expand)) = todo.pop() {
            let Some(node) = scan.node(id) else {
                continue;
            };
            if nodes.insert(id, node.clone()).is_some() && (!expand || expanded.contains(&id)) {
                continue;
            }
            if node.kind == 4 {
                if expand && expanded.insert(id) {
                    todo.extend(
                        node.data["children"]
                            .array()
                            .iter()
                            .filter_map(Fields::id)
                            .map(|id| (id, true)),
                    );
                }
                todo.extend(node.data["go"].id().map(|id| (id, false)));
                // External joints need their ancestry, but no unrelated siblings.
                if !expand {
                    todo.extend(node.data["parent"].id().map(|id| (id, false)));
                }
            } else if node.kind == 1 {
                todo.extend(
                    node.data["components"]
                        .array()
                        .iter()
                        .filter_map(|c| c[1].id())
                        .map(|id| (id, false)),
                );
            } else if node.kind == 137 {
                todo.extend(
                    node.data["m_Bones"]
                        .array()
                        .iter()
                        .filter_map(|v| v["r"].id())
                        .map(|id| (id, false)),
                );
            }
        }
        transforms.retain(|_, id| nodes.contains_key(id));
        transforms.shrink_to_fit();

        Self {
            nodes,
            transforms,
            paths: scan.paths.keys().cloned().collect(),
        }
    }

    fn transform(&self, go: u32) -> Option<u32> {
        self.transforms.get(&go).copied()
    }

    fn hierarchy(&self, root: u32) -> Vec<u32> {
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(node) = self.nodes.get(&id) else {
                continue;
            };
            ids.push(id);
            stack.extend(
                node.data["children"]
                    .array()
                    .iter()
                    .rev()
                    .filter_map(Fields::id),
            );
        }
        let mut joints = Vec::new();
        for id in &ids {
            if let Some(go) = self.nodes[id].data["go"]
                .id()
                .and_then(|id| self.nodes.get(&id))
            {
                for c in go.data["components"].array() {
                    if c[0].integer() == 137
                        && let Some(renderer) = c[1].id().and_then(|id| self.nodes.get(&id))
                    {
                        joints.extend(
                            renderer.data["m_Bones"]
                                .array()
                                .iter()
                                .filter_map(|r| r["r"].id()),
                        );
                    }
                }
            }
        }
        for mut joint in joints {
            while seen.insert(joint) {
                let Some(node) = self.nodes.get(&joint) else {
                    break;
                };
                ids.push(joint);
                let Some(parent) = node.data["parent"].id() else {
                    break;
                };
                joint = parent;
            }
        }
        ids
    }

    fn world(&self, id: u32, selected: &HashMap<u32, usize>) -> Result<DMat4> {
        let mut current = Some(id);
        let mut seen = HashSet::new();
        let mut matrix = DMat4::IDENTITY;
        while let Some(id) = current.filter(|id| selected.contains_key(id)) {
            ensure!(seen.insert(id), "Cyclic prefab hierarchy");
            let node = &self.nodes[&id];
            matrix = skin::trs(&node.data) * matrix;
            current = node.data["parent"].id();
        }
        ensure!(matrix.is_finite(), "Nonfinite prefab transform");
        Ok(matrix)
    }

    fn renderer_mesh(&self, go: &Node, renderer: &Node) -> Option<u32> {
        if renderer.kind == 137 {
            return rawref(&renderer.data, "m_Mesh");
        }
        go.data["components"]
            .array()
            .iter()
            .filter(|c| c[0].integer() == 33)
            .filter_map(|c| c[1].id().and_then(|id| self.nodes.get(&id)))
            .find_map(|n| n.data["mesh"].id())
    }
}

fn finite_json(value: &Value) -> Json {
    match value {
        Value::Map(entries) => Json::Object(
            entries
                .iter()
                .map(|(k, v)| {
                    (
                        k.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| k.integer().to_string()),
                        finite_json(v),
                    )
                })
                .collect(),
        ),
        Value::Array(values) => json!(values.iter().map(finite_json).collect::<Vec<_>>()),
        Value::Binary(bytes) => json!(bytes),
        _ => crate::value::json(value).unwrap_or(Json::Null),
    }
}

struct Exporter<'a> {
    index: &'a Index,
    plan: &'a Plan,
    output: &'a Path,
    bindings: HashMap<u32, Value>,
    clips: HashMap<u32, Arc<Clip>>,
    geometry: HashMap<u32, Json>,
}

impl Exporter<'_> {
    fn clip(&mut self, id: u32) -> Result<Arc<Clip>> {
        if let Some(clip) = self.clips.get(&id) {
            return Ok(clip.clone());
        }
        let node = self
            .plan
            .objects
            .get(&id)
            .context("Clip is absent from selected resources")?;
        let raw = unpack(&fs::read(self.output.join(node.data["file"].text()))?)?;
        let clip = Arc::new(Clip::decode(&raw)?);
        self.clips.insert(id, clip.clone());
        Ok(clip)
    }

    fn model(&mut self, root: u32) -> Result<Json> {
        let mut scene = self.scene(root)?;
        let clips = self.animations(&mut scene)?;
        scene.writer.json["extras"]["object"] = root.into();
        scene.writer.json["extras"]["kind"] = if scene.rig_count > 0 {
            "player"
        } else {
            "creature"
        }
        .into();
        scene.writer.json["extras"]["renderers"] = json!(scene.renderers);
        scene.writer.json["extras"]["warnings"] = json!(scene.warnings);

        let file = scene.writer.save(self.output, "models/prefabs")?;
        Ok(json!({"file":file,"clips":clips,"renderers":scene.renderers,"warnings":scene.warnings}))
    }
}

pub fn export(index: &Index, plan: &mut Plan, output: &Path) -> Result<()> {
    let bindings = unpack(&fs::read(output.join("animations/bindings.msgpack"))?)?;
    let bindings = bindings
        .as_map()
        .unwrap()
        .iter()
        .filter_map(|(id, data)| id.id().map(|id| (id, data.clone())))
        .collect();
    let mut exporter = Exporter {
        index,
        plan,
        output,
        bindings,
        clips: HashMap::new(),
        geometry: HashMap::new(),
    };
    let roots: BTreeSet<_> = plan
        .catalogs
        .iter()
        .map(|e| e.object)
        .filter(|id| index.transform(*id).is_some())
        .collect();
    let mut progress = Progress::for_path(output, "finish/prefabs", roots.len(), "models");
    let mut entries = BTreeMap::new();
    let mut failed = Vec::new();
    let mut issues = 0;
    for (i, root) in roots.into_iter().enumerate() {
        match exporter.model(root) {
            Ok(entry) => {
                issues += entry["warnings"].as_array().unwrap().len();
                entries.insert(root, entry);
            }
            Err(error) => {
                failed.push(json!({"object":root,"error":format!("{error:#}")}));
            }
        }
        progress.update(i + 1, format!("warnings={issues} failed={}", failed.len()));
    }
    drop(exporter);
    for (id, entry) in &entries {
        let model = plan.models.entry(*id).or_insert_with(|| map([]));
        model.set("prefab", serial(entry));
    }
    let coverage = map([
        ("models", entries.len().into()),
        ("warnings", issues.into()),
        ("failed", serial(&failed)),
    ]);
    plan.coverage.set("prefabs", coverage);
    write_json(&output.join("metadata/coverage.json"), &plan.coverage)?;
    write_json(&output.join("models/prefabs/index.json"), &serial(&entries))?;
    progress.finish();
    Ok(())
}
