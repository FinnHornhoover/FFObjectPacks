use crate::{
    animation::Bindings,
    scan::Scan,
    unity::tree::Tree,
    value::{Fields, array, map, write_json, write_pack},
};
use anyhow::Result;
use rmpv::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub fn classify(tree: &Tree) -> Option<&'static str> {
    let fields: BTreeSet<_> = tree
        .children
        .iter()
        .map(|field| field.name.as_str())
        .collect();
    if fields.contains("TableElement")
        && contains(tree, "strHairMesh")
        && contains(tree, "strFaceMesh")
    {
        Some("HNpcTableScript")
    } else if fields.contains("cSkinColor") && fields.contains("cHairColor") {
        Some("AvatarPalette")
    } else {
        None
    }
}

fn contains(tree: &Tree, name: &str) -> bool {
    tree.name == name || tree.children.iter().any(|field| contains(field, name))
}

fn aliases(scan: &Scan) -> BTreeMap<String, BTreeSet<u32>> {
    let mut aliases = BTreeMap::<String, BTreeSet<u32>>::new();
    for (path, id) in &scan.prefabs {
        if let Some(id) = id.filter(|id| scan.node(*id).is_some()) {
            aliases
                .entry(path.to_ascii_lowercase())
                .or_default()
                .insert(id);
        }
    }
    aliases
}

fn paths(row: &Value) -> BTreeMap<String, String> {
    let mut paths = BTreeMap::new();
    let mut populated = false;
    for (key, value) in row.as_map().into_iter().flatten() {
        let name = value.text();
        if !key.text().starts_with("str") || name.is_empty() || name.eq_ignore_ascii_case("null") {
            continue;
        }
        let folder = if key.text().contains("Mesh") {
            "wear"
        } else if key.text().contains("Texture") {
            "texture"
        } else {
            continue;
        };
        paths.insert(
            key.text().to_owned(),
            format!("{folder}/{name}").to_ascii_lowercase(),
        );
        populated = true;
    }
    if populated {
        paths.insert(
            "actor".into(),
            if row["iType"].integer() % 2 == 0 {
                "actor/w.kfm"
            } else {
                "actor/m.kfm"
            }
            .into(),
        );
        paths.insert(
            "skin".into(),
            if row["iType"].integer() % 2 == 0 {
                "texture/f_skin.dds"
            } else {
                "texture/m_skin.dds"
            }
            .into(),
        );
    }
    paths
}

pub fn dependencies(scan: &Scan) -> BTreeSet<u32> {
    let aliases = aliases(scan);
    scan.appearances
        .iter()
        .filter_map(|id| scan.node(*id))
        .flat_map(|node| node.data["TableElement"].array())
        .flat_map(paths)
        .filter_map(|(_, path)| aliases.get(&path))
        .flatten()
        .copied()
        .collect()
}

pub fn export(scan: &Scan, bindings: &Bindings, output: &Path) -> Result<Value> {
    let aliases = aliases(scan);
    let mut tables: Vec<(Value, Value)> = Vec::new();
    let mut palettes: Vec<(Value, Value)> = Vec::new();
    let (mut missing, mut ambiguous) = (Vec::new(), Vec::new());
    let mut rows = 0usize;

    for id in &scan.appearances {
        let node = scan.node(*id).unwrap();
        let data = &node.data;
        let mut descriptor = map([
            ("name", data["m_Name"].clone()),
            ("asset", node.asset.into()),
            (
                "source_paths",
                array(
                    scan.prefabs
                        .iter()
                        .filter(|(_, object)| *object == Some(*id))
                        .map(|(path, _)| path.clone()),
                ),
            ),
        ]);
        if data["appearance_type"].text() == "HNpcTableScript" {
            descriptor.set("rows", data["TableElement"].clone());
            let mut resolved = BTreeMap::new();
            for (row_index, row) in data["TableElement"].array().iter().enumerate() {
                rows += 1;
                for (field, path) in paths(row) {
                    let matches = aliases.get(&path).cloned().unwrap_or_default();
                    let issue = map([
                        ("table", (*id).into()),
                        ("row", row_index.into()),
                        ("field", field.into()),
                        ("path", path.clone().into()),
                        ("objects", array(matches.iter().copied())),
                    ]);
                    if matches.is_empty() {
                        missing.push(issue);
                    } else if matches.len() > 1 {
                        ambiguous.push(issue);
                    }
                    resolved.insert(path, array(matches));
                }
            }
            descriptor.set(
                "dependencies",
                Value::Map(
                    resolved
                        .into_iter()
                        .map(|(path, ids)| (path.into(), ids))
                        .collect(),
                ),
            );
            tables.push(((*id).into(), descriptor));
        } else {
            descriptor.set("skin", data["cSkinColor"]["skinColor"].clone());
            descriptor.set("hair", data["cHairColor"]["hairColor"].clone());
            descriptor.set("eye", data["cEyeColor"]["eyeColor"].clone());
            palettes.push(((*id).into(), descriptor));
        }
    }

    let mut avatars = Vec::new();
    for (path, id) in &scan.prefabs {
        if !["actor/m.kfm", "actor/w.kfm"].contains(&path.to_ascii_lowercase().as_str()) {
            continue;
        }
        let Some(id) = id else { continue };
        let model = bindings.models.get(id).unwrap_or(&Value::Nil);
        let rigs: Vec<_> = model["rigs"]
            .array()
            .iter()
            .filter_map(Fields::id)
            .filter(|id| {
                bindings
                    .rigs
                    .get(id)
                    .is_some_and(|rig| rig["script_type"].text() == "ActorSkinCombiner")
            })
            .collect();
        let names: BTreeSet<_> = model["animations"]
            .array()
            .iter()
            .filter_map(Fields::id)
            .filter_map(|id| bindings.animations.get(&id))
            .flat_map(|binding| {
                binding["clips"]
                    .array()
                    .iter()
                    .filter_map(Fields::id)
                    .chain(binding["default_clip"].id())
            })
            .filter_map(|id| scan.node(id))
            .map(|node| node.data["name"].text().to_ascii_lowercase())
            .collect();
        avatars.push(map([
            ("source_path", path.clone().into()),
            ("object", (*id).into()),
            ("rigs", array(rigs.iter().copied())),
            ("missing_actor_rig", rigs.is_empty().into()),
            (
                "missing_adjustment_clips",
                array(["height", "shape"].into_iter().filter(|name| {
                    !names.contains(*name) && !names.contains(&format!("{name}_add"))
                })),
            ),
        ]));
    }

    let mut invalid_colours = Vec::new();
    for (field, palette, runtime) in [
        ("iSkinColor", "skin", "skinColor"),
        ("iHairColor", "hair", "hairColor"),
    ] {
        let limit = palettes
            .iter()
            .map(|(_, value)| value[palette].array().len())
            .chain(
                scan.runtime_palettes
                    .iter()
                    .filter(|record| {
                        record["field"].text() == runtime && record["supported"].truth()
                    })
                    .map(|record| record["values"].array().len()),
            )
            .max()
            .unwrap_or(0);
        for (id, table) in &tables {
            for (row, data) in table["rows"].array().iter().enumerate() {
                if limit > 0 && data[field].integer() >= limit as i64 {
                    invalid_colours.push(map([
                        ("table", id.clone()),
                        ("row", row.into()),
                        ("field", field.into()),
                        ("index", data[field].clone()),
                        ("available", limit.into()),
                    ]));
                }
            }
        }
    }

    write_pack(
        &output.join("models/appearances/hnpcs.msgpack"),
        &Value::Map(tables.clone()),
    )?;
    write_pack(
        &output.join("models/appearances/palettes.msgpack"),
        &Value::Map(palettes.clone()),
    )?;
    write_pack(
        &output.join("models/appearances/runtime.msgpack"),
        &array(scan.runtime_palettes.clone()),
    )?;
    write_json(
        &output.join("models/appearances/index.json"),
        &map([
            ("hnpcs", "models/appearances/hnpcs.msgpack".into()),
            ("palettes", "models/appearances/palettes.msgpack".into()),
            ("runtime", "models/appearances/runtime.msgpack".into()),
        ]),
    )?;

    Ok(map([
        ("tables", tables.len().into()),
        ("rows", rows.into()),
        ("palettes", palettes.len().into()),
        ("runtime_palettes", scan.runtime_palettes.len().into()),
        ("missing_paths", array(missing)),
        ("ambiguous_paths", array(ambiguous)),
        ("runtime_errors", array(scan.runtime_errors.clone())),
        (
            "runtime_unsupported",
            array(
                scan.runtime_palettes
                    .iter()
                    .filter(|record| record["supported"] == Value::Boolean(false))
                    .cloned(),
            ),
        ),
        ("missing_tables", tables.is_empty().into()),
        ("missing_palettes", palettes.is_empty().into()),
        ("invalid_colour_indices", array(invalid_colours)),
        ("avatars", array(avatars)),
    ]))
}
