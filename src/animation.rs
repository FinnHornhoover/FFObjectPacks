use crate::{
    scan::Scan,
    value::{Fields, array, map, rawref, serial, write_json, write_pack},
};
use anyhow::Result;
use rmpv::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::Path,
};

pub struct Bindings {
    pub animations: BTreeMap<u32, Value>,
    pub rigs: BTreeMap<u32, Value>,
    pub models: HashMap<u32, Value>,
    pub coverage: Value,
}

fn link(
    scan: &Scan,
    selected: &BTreeSet<u32>,
    go_transforms: &HashMap<u32, u32>,
    models: &mut HashMap<u32, Value>,
    oid: u32,
    mut go: Option<u32>,
    kind: &str,
) {
    let mut transform = go.and_then(|go| go_transforms.get(&go).copied());
    let mut seen = HashSet::new();
    while let Some(id) = go {
        if !seen.insert(id) {
            break;
        }

        let entry = models.entry(id).or_insert_with(|| {
            map([
                ("animations", array(Vec::<Value>::new())),
                ("rigs", array(Vec::<Value>::new())),
            ])
        });
        let mut list = entry[kind].array().to_vec();
        list.push(oid.into());
        entry.set(kind, array(list));

        let Some(t) = transform else {
            break;
        };
        let parent = scan.node(t).and_then(|n| n.data["parent"].id());
        let Some(parent) =
            parent.filter(|p| selected.contains(p) && scan.node(*p).is_some_and(|n| n.kind == 4))
        else {
            break;
        };
        transform = Some(parent);
        go = scan.node(parent).and_then(|n| n.data["go"].id());
    }
}

pub fn collect(scan: &Scan, selected: &BTreeSet<u32>, output: &Path) -> Result<Bindings> {
    let clips: BTreeMap<_, _> = selected
        .iter()
        .filter_map(|id| {
            scan.node(*id)
                .filter(|n| n.kind == 74)
                .map(|n| (*id, &n.data))
        })
        .collect();

    let mut go_transforms = HashMap::new();
    let mut children: HashMap<Option<u32>, HashMap<String, Vec<u32>>> = HashMap::new();
    for id in selected {
        let node = scan.node(*id).unwrap();
        if node.kind != 4 {
            continue;
        }
        let go = node.data["go"].id();
        if let Some(go) = go {
            go_transforms.insert(go, *id);
        }
        let name = go
            .and_then(|go| scan.node(go))
            .map(|n| n.data["name"].text())
            .unwrap_or("");
        children
            .entry(node.data["parent"].id())
            .or_default()
            .entry(name.to_owned())
            .or_default()
            .push(*id);
    }

    let (mut animations, mut rigs, mut models) = (BTreeMap::new(), BTreeMap::new(), HashMap::new());
    let mut used = BTreeSet::new();
    let (mut missing_clips, mut unresolved_paths, mut ambiguous_paths) =
        (Vec::new(), Vec::new(), Vec::new());
    for id in selected {
        let node = scan.node(*id).unwrap();
        let data = &node.data;
        if node.kind != 111 && data["script_type"].is_nil() {
            continue;
        }

        let go = rawref(data, "m_GameObject");
        let root = go.and_then(|id| go_transforms.get(&id).copied());
        if node.kind != 111 {
            rigs.insert(
                *id,
                map([
                    ("script_type", data["script_type"].clone()),
                    ("game_object", go.map(Value::from).unwrap_or(Value::Nil)),
                    ("transform", root.map(Value::from).unwrap_or(Value::Nil)),
                    ("data", data.without(&["script_type"])),
                ]),
            );
            link(scan, selected, &go_transforms, &mut models, *id, go, "rigs");
            continue;
        }

        let default = rawref(data, "m_Animation");
        let references: Vec<_> = data["m_Animations"]
            .array()
            .iter()
            .map(|r| r["r"].clone())
            .collect();
        let mut clip_ids = Vec::new();
        for clip in references.iter().filter_map(Fields::id).chain(default) {
            if !clip_ids.contains(&clip) {
                clip_ids.push(clip);
            }
        }
        used.extend(clip_ids.iter().copied());
        let missing: Vec<_> = clip_ids
            .iter()
            .copied()
            .filter(|id| !clips.contains_key(id))
            .collect();
        if !missing.is_empty() {
            missing_clips.push(map([("binding", (*id).into()), ("clips", array(missing))]));
        }

        let paths: BTreeSet<u32> = clip_ids
            .iter()
            .filter_map(|id| clips.get(id))
            .flat_map(|clip| clip["paths"].array().iter().filter_map(Fields::id))
            .collect();
        let (mut targets, mut unresolved, mut ambiguous) = (Vec::new(), Vec::new(), Vec::new());
        for path_id in paths {
            let path = scan.paths.get_index(path_id as usize).unwrap().0;
            let mut matches = root.into_iter().collect::<Vec<_>>();
            if !path.is_empty() {
                for part in path.split('/') {
                    matches = matches
                        .iter()
                        .flat_map(|parent| {
                            children
                                .get(&Some(*parent))
                                .and_then(|names| names.get(part))
                                .into_iter()
                                .flatten()
                                .copied()
                        })
                        .collect();
                    if matches.is_empty() {
                        break;
                    }
                }
            }

            match matches.len() {
                0 => unresolved.push(path_id),
                1 => targets.push(array([Value::from(path_id), matches[0].into()])),
                _ => ambiguous.push(array([Value::from(path_id), array(matches)])),
            }
        }

        if !unresolved.is_empty() {
            unresolved_paths.push(map([
                ("binding", (*id).into()),
                ("paths", array(unresolved.iter().copied())),
            ]));
        }
        if !ambiguous.is_empty() {
            ambiguous_paths.push(map([
                ("binding", (*id).into()),
                ("paths", array(ambiguous.clone())),
            ]));
        }

        animations.insert(
            *id,
            map([
                ("game_object", go.map(Value::from).unwrap_or(Value::Nil)),
                ("transform", root.map(Value::from).unwrap_or(Value::Nil)),
                (
                    "default_clip",
                    default.map(Value::from).unwrap_or(Value::Nil),
                ),
                ("clips", array(references)),
                (
                    "settings",
                    data.without(&["m_GameObject", "m_Animation", "m_Animations"]),
                ),
                ("targets", array(targets)),
                ("unresolved_paths", array(unresolved)),
                ("ambiguous_paths", array(ambiguous)),
            ]),
        );
        link(
            scan,
            selected,
            &go_transforms,
            &mut models,
            *id,
            go,
            "animations",
        );
    }

    let mut aliases: HashMap<u32, Vec<String>> = HashMap::new();
    for (path, id) in &scan.prefabs {
        if let Some(id) = id.filter(|id| clips.contains_key(id)) {
            let entries = aliases.entry(id).or_default();
            if !entries.contains(path) {
                entries.push(path.clone());
            }
        }
    }

    let mut index = Vec::new();
    for (id, data) in &clips {
        let mut entry = map([
            ("object", (*id).into()),
            ("asset", (scan.node(*id).unwrap().asset as u64).into()),
        ]);
        entry
            .as_map_mut()
            .unwrap()
            .extend(data.as_map().unwrap().clone());
        entry.set(
            "source_paths",
            array(aliases.remove(id).unwrap_or_default()),
        );
        index.push(entry);
    }

    write_json(
        &output.join("animations/index.json"),
        &map([
            ("paths", array(scan.paths.keys().cloned())),
            ("bindings", "animations/bindings.msgpack".into()),
            ("clips", array(index)),
        ]),
    )?;
    write_pack(
        &output.join("animations/bindings.msgpack"),
        &serial(&animations),
    )?;
    write_pack(&output.join("models/rigs.msgpack"), &serial(&rigs))?;

    let coverage = map([
        ("clips", (clips.len() as u64).into()),
        ("bindings", (animations.len() as u64).into()),
        ("rigs", (rigs.len() as u64).into()),
        (
            "unbound_clips",
            array(clips.keys().filter(|id| !used.contains(id)).copied()),
        ),
        ("missing_clips", array(missing_clips)),
        ("unresolved_paths", array(unresolved_paths)),
        ("ambiguous_paths", array(ambiguous_paths)),
    ]);

    Ok(Bindings {
        animations,
        rigs,
        models,
        coverage,
    })
}
