use crate::{
    scan::Scan,
    unity::tree::Tree,
    value::{Fields, map, serial, write_pack},
};
use anyhow::Result;
use std::{collections::BTreeMap, path::Path};

pub fn classify(tree: &Tree) -> Option<&'static str> {
    let has = |name: &str| tree.children.iter().any(|field| field.name == name);
    if has("fogColor") && has("skyColor") && has("lightColor") {
        Some("DongColorSetup")
    } else if has("attributes") {
        Some("MapAttributeTable")
    } else if has("moveType") && has("velocity") && has("from") && has("to") {
        Some("EpPlatformTrigger")
    } else if has("point") && has("next") {
        Some("Waypoint")
    } else if has("effectName") && has("nifObject") {
        Some("EffectEmitterController")
    } else {
        None
    }
}

pub fn export(scan: &Scan, output: &Path) -> Result<()> {
    let records: BTreeMap<_, _> = scan
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(id, node)| {
            let node = node.as_ref()?;
            let kind = if !node.data["world_type"].is_nil() {
                node.data["world_type"].text()
            } else if node.kind == 108 {
                "Light"
            } else if node.kind == 104 {
                "RenderSettings"
            } else {
                return None;
            };
            Some((
                id as u32,
                map([
                    ("type", kind.into()),
                    ("asset", (node.asset as u64).into()),
                    ("data", node.data.clone()),
                ]),
            ))
        })
        .collect();
    write_pack(&output.join("areas/world.msgpack"), &serial(&records))
}
