use crate::{
    cache::Cache,
    convert::{self, Store},
    plan::{self, Plan},
    scan::Scan,
    value::{Fields, array, map, serial, write, write_json, write_pack},
};
use anyhow::Result;
use rmpv::Value;
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::Path,
};

pub fn extract(
    build: &str,
    revision: u32,
    nickname: Option<&str>,
    cache: &Path,
    output: &Path,
    store: &Store,
    work: Option<&Path>,
) -> Result<()> {
    let prepared = prepare(cache, output, work)?;
    finish(build, revision, nickname, prepared, output, store)
}

pub struct Prepared {
    cache: Cache,
    scan: Scan,
}

pub fn prepare(cache: &Path, output: &Path, work: Option<&Path>) -> Result<Prepared> {
    if output.exists() {
        fs::remove_dir_all(output)?;
    }
    fs::create_dir_all(output)?;

    let cache = Cache::open(cache)?;
    let scan = Scan::run(&cache, output, work)?;
    Ok(Prepared { cache, scan })
}

pub fn finish(
    build: &str,
    revision: u32,
    nickname: Option<&str>,
    prepared: Prepared,
    output: &Path,
    store: &Store,
) -> Result<()> {
    let Prepared { cache, mut scan } = prepared;
    let mut planned = plan::build(&scan, output)?;
    drop(std::mem::take(&mut scan.nodes));

    let exported = convert::export(&cache, &scan, &planned.selection, output, store)?;
    for (id, result) in exported {
        planned
            .objects
            .get_mut(&id)
            .unwrap()
            .data
            .as_map_mut()
            .unwrap()
            .extend(result.as_map().unwrap().clone());
    }

    catalogs(&planned, output)?;
    for (materials, path) in [
        (true, "metadata/materials.msgpack"),
        (false, "metadata/resources.msgpack"),
    ] {
        let objects = Value::Map(
            planned
                .objects
                .iter()
                .filter(|(_, n)| (n.kind == 21) == materials)
                .map(|(id, n)| {
                    (
                        (*id).into(),
                        array([Value::from(n.kind), (n.asset as u64).into(), n.data.clone()]),
                    )
                })
                .collect(),
        );
        write_pack(&output.join(path), &objects)?;
    }

    write_pack(
        &output.join("metadata/sources.msgpack"),
        &map([
            ("assets", serial(&scan.assets)),
            (
                "keys",
                array((0..scan.keys.len()).map(|id| scan.source_key(id as u32))),
            ),
        ]),
    )?;

    write_json(
        &output.join("metadata/graphs.json"),
        &array(planned.graphs.clone()),
    )?;

    let coordinates = map([
        (
            "hierarchies",
            "Unity Y-up left-handed source TRS".into()
        ),
        (
            "geometry",
            "glTF right-handed, reflected X; reverse winding; tangent X and W negated".into()
        ),
        (
            "conversion",
            "Reflect hierarchy positions X and quaternion Y/Z when building a glTF scene; conjugate bind matrices by diag(-1,1,1,1)".into()
        ),
        (
            "uv",
            "Source Unity UVs; texture images vertically flipped from Unity storage; use flipY=false with glTF".into()
        ),
        (
            "terrain",
            "Original sample order, dimensions, scale and signedness; reconstruct in source coordinates before conversion".into()
        )
    ]);
    let mut manifest = map([
        ("format_version", crate::FORMAT_VERSION.into()),
        ("build", build.into()),
        ("revision", revision.into()),
        ("nickname", nickname.map(Value::from).unwrap_or(Value::Nil)),
        (
            "capabilities",
            map([
                ("geometry", "unquantized GLB".into()),
                ("textures", "lossless WebP".into()),
                ("terrain", true.into()),
                ("skin_source_data", true.into()),
                ("animations", true.into()),
                ("animation_encoding", "Unity legacy source curves".into()),
                ("rig_bindings", true.into()),
                ("appearance_metadata", true.into()),
                ("shader_metadata", true.into()),
            ]),
        ),
        ("coordinates", coordinates),
    ]);
    for (key, path) in [
        ("models", "models/index.json"),
        ("areas", "areas/index.json"),
        ("animations", "animations/index.json"),
        ("rigs", "models/rigs.msgpack"),
        ("appearances", "models/appearances/index.json"),
        ("materials", "metadata/materials.msgpack"),
        ("resources", "metadata/resources.msgpack"),
        ("graphs", "metadata/graphs.json"),
        ("node_map", "metadata/node-map.bin"),
        ("source_objects", "metadata/sources.msgpack"),
        ("coverage", "metadata/coverage.json"),
    ] {
        manifest.set(key, path.into());
    }

    let cache_manifest = cache.root.join(".cache-manifest.json");
    if cache_manifest.exists() {
        let version: serde_json::Value = serde_json::from_slice(&fs::read(&cache_manifest)?)?;
        manifest.set(
            "cache_uuid",
            serial(version.get("uuid").unwrap_or(&serde_json::Value::Null)),
        );
        write_json(
            &output.join("metadata/cache-manifest.json"),
            &serial(&version),
        )?;
        let files = cache.root.join(".cache-files.json");
        if files.exists() {
            let files: serde_json::Value = serde_json::from_slice(&fs::read(files)?)?;
            write_json(&output.join("metadata/cache-files.json"), &serial(&files))?;
        }
    }

    write_json(&output.join("manifest.json"), &manifest)?;
    write(
        &output.join("README.md"),
        b"# FusionFall Object Pack\n\nStart with manifest.json and the models/ and areas/ indexes.\nUse animations/ for source clips and bindings, models/rigs.msgpack for clothing maps, and models/skins/ for weights and bind poses.\nFormat: https://github.com/FinnHornhoover/FFObjectPacks/blob/main/FORMAT.md\n"
    )?;
    Ok(())
}

fn catalogs(plan: &Plan, output: &Path) -> Result<()> {
    let mut categories: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut seen = HashSet::new();
    for entry in &plan.catalogs {
        if !seen.insert((&entry.path, entry.object)) {
            continue;
        }

        let id = entry.object;
        let mut description = map([
            ("source_path", entry.path.clone().into()),
            ("object", id.into()),
        ]);
        if let Some((graph, node)) = plan.node_map.get(&id) {
            description.set("graph", plan.graphs[*graph]["file"].clone());
            description.set("root_node", (*node as u64).into());
            if let Some(bindings) = plan.models.get(&id) {
                description
                    .as_map_mut()
                    .unwrap()
                    .extend(bindings.as_map().unwrap().clone());
            }
        } else if let Some(node) = plan.objects.get(&id) {
            description
                .as_map_mut()
                .unwrap()
                .extend(node.data.as_map().unwrap().clone());
        } else {
            continue;
        }

        let index = if entry.category == "textures" {
            description
        } else {
            let name = entry.path.rsplit('/').next().unwrap_or("");
            let basename: String = name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let file = format!("models/{}/{basename}-{id}.json", entry.category);
            write_json(&output.join(&file), &description)?;
            map([
                ("source_path", entry.path.clone().into()),
                ("file", file.into()),
                ("object", id.into()),
            ])
        };
        categories
            .entry(entry.category.clone())
            .or_default()
            .push(index);
    }

    for (category, entries) in &mut categories {
        entries.sort_by(|a, b| {
            a["source_path"]
                .text()
                .cmp(b["source_path"].text())
                .then_with(|| a["object"].integer().cmp(&b["object"].integer()))
        });
        let folder = if category == "textures" {
            "textures".to_owned()
        } else {
            format!("models/{category}")
        };
        write_json(
            &output.join(format!("{folder}/index.json")),
            &array(entries.clone()),
        )?;
    }

    write_json(
        &output.join("models/index.json"),
        &Value::Map(
            categories
                .keys()
                .filter(|category| *category != "textures")
                .map(|category| {
                    (
                        category.clone().into(),
                        format!("models/{category}/index.json").into(),
                    )
                })
                .collect(),
        ),
    )?;

    write_json(
        &output.join("areas/index.json"),
        &map([
            (
                "scenes",
                array(
                    plan.graphs
                        .iter()
                        .filter(|g| g["asset"]["scene"].truth())
                        .cloned(),
                ),
            ),
            (
                "unavailable_scenes",
                plan.coverage["unavailable_scenes"].clone(),
            ),
        ]),
    )
}
