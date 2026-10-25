use crate::{
    animation,
    progress::Progress,
    scan::{Node, Scan},
    value::{Fields, array, map, rawref, refs, serial, write, write_json, write_pack},
};
use anyhow::Result;
use rmpv::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::Path,
};

#[derive(Clone)]
pub struct Catalog {
    pub path: String,
    pub object: u32,
    pub category: String,
}

#[derive(Clone)]
pub struct Selection {
    pub object: u32,
    pub kind: i32,
    pub asset: usize,
    pub path: i64,
}

pub struct Plan {
    pub graphs: Vec<Value>,
    pub objects: BTreeMap<u32, Node>,
    pub catalogs: Vec<Catalog>,
    pub selection: Vec<Selection>,
    pub coverage: Value,
    pub models: HashMap<u32, Value>,
    pub node_map: HashMap<u32, (usize, usize)>,
}

fn category(path: &str) -> &'static str {
    let path = path.to_ascii_lowercase();
    let name = path.rsplit('/').next().unwrap_or("");

    if path.starts_with("nano/") {
        "nanos"
    } else if path.starts_with("wear/") {
        "items"
    } else if path.starts_with("actor/") {
        "players"
    } else if name.starts_with("npc_") {
        "npcs"
    } else if path.starts_with("mob/") && name.starts_with("mob_") {
        "monsters"
    } else {
        "props"
    }
}

fn add_tree(
    scan: &Scan,
    children: &HashMap<u32, Vec<u32>>,
    expanded: &mut HashSet<u32>,
    todo: &mut Vec<u32>,
    go: Option<u32>,
) {
    let Some(go) =
        go.filter(|go| !expanded.contains(go) && scan.node(*go).is_some_and(|n| n.kind == 1))
    else {
        return;
    };

    expanded.insert(go);
    let mut stack: Vec<_> = scan.node(go).unwrap().data["components"]
        .array()
        .iter()
        .filter(|c| c[0].integer() == 4)
        .filter_map(|c| c[1].id())
        .collect();
    let mut visited = HashSet::new();
    while let Some(id) = stack.pop() {
        if !visited.insert(id) {
            continue;
        }
        todo.push(id);
        if let Some(node) = scan.node(id) {
            todo.extend(node.data["go"].id());
        }
        if let Some(entries) = children.get(&id) {
            stack.extend(entries);
        }
    }
}

pub fn build(scan: &Scan, output: &Path) -> Result<Plan> {
    let mut progress = Progress::new("resolve dependencies", scan.nodes.len());
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (id, node) in scan
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| n.as_ref().map(|n| (i, n)))
    {
        if node.kind == 4
            && let Some(parent) = node.data["parent"].id()
        {
            children.entry(parent).or_default().push(id as u32);
        }
    }

    let (mut usable, mut unavailable) = (Vec::new(), Vec::new());
    for (aid, roots) in &scan.scenes {
        let missing: Vec<_> = roots
            .iter()
            .copied()
            .filter(|id| {
                scan.node(*id).is_some_and(|n| {
                    n.kind == 154
                        && rawref(&n.data, "m_TerrainData")
                            .and_then(|id| scan.node(id))
                            .is_none()
                })
            })
            .collect();
        if missing.is_empty() {
            usable.push(roots);
        } else {
            unavailable.push(map([
                ("asset", serial(&scan.assets[*aid])),
                (
                    "reason",
                    "Terrain bundles referenced by this scene are absent from the cache".into(),
                ),
                ("colliders", array(missing)),
            ]));
        }
    }

    let mut todo: Vec<u32> = usable.into_iter().flatten().copied().collect();
    todo.extend(&scan.terrains);
    todo.extend(&scan.clips);
    todo.extend(&scan.rigs);
    let (mut expanded, mut catalogs) = (HashSet::new(), Vec::new());
    for (path, id) in &scan.prefabs {
        let Some(id) = id else {
            continue;
        };
        let Some(node) = scan.node(*id) else {
            continue;
        };
        if node.kind == 1 {
            add_tree(scan, &children, &mut expanded, &mut todo, Some(*id));
            catalogs.push(Catalog {
                path: path.clone(),
                object: *id,
                category: category(path).into(),
            });
        } else if [21, 27, 28, 43].contains(&node.kind)
            && ["texture/", "mob/", "nano/", "wear/", "actor/"]
                .iter()
                .any(|p| path.to_ascii_lowercase().starts_with(p))
        {
            todo.push(*id);
            catalogs.push(Catalog {
                path: path.clone(),
                object: *id,
                category: if [27, 28].contains(&node.kind) {
                    "textures"
                } else {
                    category(path)
                }
                .into(),
            });
        }
    }

    let (mut seen, mut missing) = (HashSet::new(), BTreeSet::new());
    while let Some(id) = todo.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(node) = scan.node(id) else {
            missing.insert(id);
            continue;
        };

        let data = &node.data;
        match node.kind {
            1 => {
                let components = data["components"].array();
                let renderer = components.iter().any(|c| c[0].integer() == 23);
                for c in components {
                    if let Some(id) = c[1]
                        .id()
                        .filter(|id| scan.node(*id).is_some() && (c[0].integer() != 33 || renderer))
                    {
                        todo.push(id);
                    }
                }
            }
            4 => {
                todo.extend(data["parent"].id());
                todo.extend(data["go"].id());
            }
            33 => todo.extend(data["mesh"].id()),
            23 | 26 | 137 => {
                refs(data, &mut todo);
                if node.kind == 23
                    && let Some(go) = rawref(data, "m_GameObject").and_then(|id| scan.node(id))
                {
                    todo.extend(
                        go.data["components"]
                            .array()
                            .iter()
                            .filter(|c| c[0].integer() == 33)
                            .filter_map(|c| c[1].id()),
                    );
                }
            }
            111 => add_tree(
                scan,
                &children,
                &mut expanded,
                &mut todo,
                rawref(data, "m_GameObject"),
            ),
            154 => {
                todo.extend(rawref(data, "m_GameObject"));
                todo.extend(rawref(data, "m_TerrainData"));
            }
            21 | 104 | 108 | 156 | 157 => {
                let mut dependencies = Vec::new();
                refs(data, &mut dependencies);
                todo.extend(&dependencies);
                if node.kind == 156 {
                    for id in dependencies {
                        add_tree(scan, &children, &mut expanded, &mut todo, Some(id));
                    }
                }
            }
            tid if tid < 0 => {
                let mut dependencies = Vec::new();
                refs(data, &mut dependencies);
                todo.extend(&dependencies);
                if !data["script_type"].is_nil() {
                    for id in dependencies {
                        add_tree(scan, &children, &mut expanded, &mut todo, Some(id));
                    }
                }
            }
            _ => {}
        }

        progress.update(seen.len(), format!("missing={}", missing.len()));
    }
    progress.finish();

    let selected: BTreeSet<_> = seen
        .into_iter()
        .filter(|id| scan.node(*id).is_some())
        .collect();

    let bindings = animation::collect(scan, &selected, output)?;
    let mut groups: BTreeMap<usize, Vec<u32>> = BTreeMap::new();
    let mut selected_types: BTreeMap<i32, usize> = BTreeMap::new();
    for id in &selected {
        let node = scan.node(*id).unwrap();
        groups.entry(node.asset).or_default().push(*id);
        *selected_types.entry(node.kind).or_default() += 1;
    }

    let (mut graphs, mut node_maps, mut objects, mut selection) =
        (Vec::new(), BTreeSet::new(), BTreeMap::new(), Vec::new());
    let mut progress = Progress::new("write scenes", groups.len());
    for (aid, ids) in groups {
        let transforms: Vec<_> = ids
            .iter()
            .copied()
            .filter(|id| scan.node(*id).unwrap().kind == 4)
            .collect();
        let transform_map: HashMap<_, _> = transforms
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, i))
            .collect();

        let mut names = indexmap::IndexMap::<String, usize>::new();
        let (mut binary, mut renderers, mut settings, mut external) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for id in &transforms {
            let data = &scan.node(*id).unwrap().data;
            let go = data["go"].id();
            let go_data = go.and_then(|go| scan.node(go)).map(|n| &n.data);
            let name = go_data.map(|d| d["name"].text()).unwrap_or("").to_owned();
            let next = names.len();
            let name_index = *names.entry(name).or_insert(next);

            let parent = data["parent"].id();
            if let Some(parent) = parent.filter(|id| !transform_map.contains_key(id)) {
                external.push(array([Value::from(*id), parent.into()]));
            }

            let active = go_data.map(|d| d["active"].truth()).unwrap_or(true);
            let layer = go_data.map(|d| d["layer"].integer()).unwrap_or(0);
            let flags = u32::from(active) | ((layer as u32) << 1);

            binary.extend(id.to_le_bytes());
            binary.extend(
                parent
                    .and_then(|id| transform_map.get(&id).copied())
                    .map(|n| n as i32)
                    .unwrap_or(-1)
                    .to_le_bytes(),
            );
            binary.extend(go.unwrap_or(u32::MAX).to_le_bytes());
            binary.extend((name_index as u32).to_le_bytes());
            for key in ["p", "q", "s"] {
                for value in data[key].array() {
                    binary.extend((value.number() as f32).to_le_bytes());
                }
            }
            binary.extend(flags.to_le_bytes());

            node_maps.insert((*id, graphs.len(), transform_map[id]));
            if let Some(go) = go {
                node_maps.insert((go, graphs.len(), transform_map[id]));
            }
        }

        for id in &ids {
            let node = scan.node(*id).unwrap();
            let data = &node.data;
            match node.kind {
                1 | 4 | 33 => {}
                23 | 26 | 137 | 154 => {
                    let components = rawref(data, "m_GameObject")
                        .and_then(|go| scan.node(go))
                        .map(|n| n.data["components"].array())
                        .unwrap_or(&[]);
                    let transform = components
                        .iter()
                        .find(|c| c[0].integer() == 4)
                        .and_then(|c| c[1].id());
                    let mesh = if node.kind == 23 {
                        components
                            .iter()
                            .filter(|c| c[0].integer() == 33)
                            .find_map(|c| c[1].id().and_then(|id| scan.node(id)))
                            .and_then(|n| n.data["mesh"].id())
                    } else {
                        rawref(data, "m_Mesh")
                    };
                    renderers.push(array([
                        Value::from(*id),
                        node.kind.into(),
                        transform.map(Value::from).unwrap_or(Value::Nil),
                        mesh.map(Value::from).unwrap_or(Value::Nil),
                        data.without(&["m_GameObject"]),
                    ]));
                }
                111 => {
                    objects.insert(
                        *id,
                        Node {
                            kind: node.kind,
                            asset: aid,
                            data: map([("binding", (*id).into())]),
                        },
                    );
                }
                tid if tid < 0 && !data["script_type"].is_nil() => {
                    objects.insert(
                        *id,
                        Node {
                            kind: tid,
                            asset: aid,
                            data: map([("rig", (*id).into())]),
                        },
                    );
                }
                104 | 108 | 157 => {
                    settings.push(array([Value::from(*id), node.kind.into(), data.clone()]))
                }
                tid if tid < 0 => {
                    settings.push(array([Value::from(*id), tid.into(), data.clone()]))
                }
                _ => {
                    objects.insert(*id, node.clone());
                    if [27, 28, 43, 89].contains(&node.kind) {
                        selection.push(Selection {
                            object: *id,
                            kind: node.kind,
                            asset: aid,
                            path: scan.keys[*id as usize].1,
                        });
                    }
                }
            }
        }

        if !transforms.is_empty() || !renderers.is_empty() || !settings.is_empty() {
            let asset = &scan.assets[aid];
            let folder = if asset.scene {
                "areas/scenes"
            } else {
                "models/hierarchies"
            };
            let file = format!("{folder}/{aid}.msgpack");
            write_pack(
                &output.join(&file),
                &map([
                    ("asset", (aid as u64).into()),
                    ("names", array(names.keys().cloned())),
                    ("node_stride", 60.into()),
                    ("nodes", Value::Binary(binary)),
                    ("renderers", array(renderers.clone())),
                    ("settings", array(settings)),
                    ("external_parents", array(external)),
                    (
                        "animations",
                        array(
                            ids.iter()
                                .filter(|id| bindings.animations.contains_key(id))
                                .copied(),
                        ),
                    ),
                    (
                        "rigs",
                        array(
                            ids.iter()
                                .filter(|id| bindings.rigs.contains_key(id))
                                .copied(),
                        ),
                    ),
                ]),
            )?;
            graphs.push(map([
                ("file", file.into()),
                ("asset", serial(asset)),
                ("nodes", (transforms.len() as u64).into()),
                ("renderers", (renderers.len() as u64).into()),
            ]));
        }
        progress.update(graphs.len(), "");
    }
    progress.finish();

    let mut binary = b"NMAP".to_vec();
    binary.extend((node_maps.len() as u32).to_le_bytes());
    let mut node_map = HashMap::new();
    for (id, graph, node) in node_maps {
        binary.extend(id.to_le_bytes());
        binary.extend(u16::try_from(graph)?.to_le_bytes());
        binary.extend(u32::try_from(node)?.to_le_bytes());
        node_map.insert(id, (graph, node));
    }
    write(&output.join("metadata/node-map.bin"), &binary)?;

    let coverage = map([
        (
            "scenes",
            (graphs
                .iter()
                .filter(|g| g["asset"]["scene"].truth())
                .count() as u64)
                .into(),
        ),
        (
            "terrain_data",
            (*selected_types.get(&156).unwrap_or(&0) as u64).into(),
        ),
        ("selected_types", serial(&selected_types)),
        (
            "missing",
            array(
                missing
                    .into_iter()
                    .map(|id| map([("object", id.into()), ("source_key", scan.source_key(id))])),
            ),
        ),
        ("unavailable_scenes", array(unavailable)),
        ("scan_errors", array(scan.errors.clone())),
        ("animations", bindings.coverage),
    ]);
    write_json(&output.join("metadata/coverage.json"), &coverage)?;

    Ok(Plan {
        graphs,
        objects,
        catalogs,
        selection,
        coverage,
        models: bindings.models,
        node_map,
    })
}
