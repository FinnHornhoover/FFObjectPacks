use super::{Document, relative};
use crate::{
    scan::Node,
    value::{Fields, rawref},
};
use anyhow::Result;
use rmpv::Value;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

fn properties(value: &Value, channels: &str) -> Json {
    if channels.is_empty() {
        return json!(value.number());
    }
    json!(
        channels
            .chars()
            .map(|c| value[c.to_string().as_str()].number())
            .collect::<Vec<_>>()
    )
}

fn entries(value: &Value) -> impl Iterator<Item = (&str, &Value)> {
    value.array().iter().map(|entry| {
        let name = if entry[0].as_str().is_some() {
            entry[0].text()
        } else {
            entry[0]["name"].text()
        };
        (name, &entry[1])
    })
}

impl Document {
    fn texture(&mut self, id: u32, objects: &BTreeMap<u32, Node>) -> Option<usize> {
        if let Some(index) = self.textures.get(&id) {
            return Some(*index);
        }
        let data = &objects.get(&id)?.data;
        let file = data["file"].as_str()?;
        let image=self.add("images",json!({"uri":relative("models/prefabs",file),"mimeType":"image/webp","name":data["name"].text()}));
        let settings = &data["settings"];
        let wrap = if settings["m_WrapMode"].integer() == 1 {
            33071
        } else {
            10497
        };
        let nearest = settings["m_FilterMode"].integer() == 0;
        let sampler=self.add("samplers",json!({"wrapS":wrap,"wrapT":wrap,"magFilter":if nearest {9728} else {9729},"minFilter":if nearest {9728} else {9987}}));
        let index = self.add(
            "textures",
            json!({"sampler":sampler,"extensions":{"EXT_texture_webp":{"source":image}}}),
        );
        self.json["extensionsUsed"] = json!(["EXT_texture_webp"]);
        self.json["extensionsRequired"] = json!(["EXT_texture_webp"]);
        self.textures.insert(id, index);
        Some(index)
    }

    pub fn material(&mut self, id: Option<u32>, objects: &BTreeMap<u32, Node>) -> Result<usize> {
        if let Some(index) = self.materials.get(&id) {
            return Ok(*index);
        }
        let raw = id.and_then(|id| objects.get(&id)).map(|n| &n.data);
        let mut material = json!({"name":"default","pbrMetallicRoughness":{"metallicFactor":0,"roughnessFactor":1}});
        if let Some(raw) = raw {
            let shader = rawref(raw, "m_Shader").and_then(|id| objects.get(&id));
            let shader_name = shader.map(|n| n.data["name"].text()).unwrap_or("");
            let mut saved = raw["m_SavedProperties"].clone();
            if let Some(shader) = shader {
                for field in ["m_Colors", "m_Floats", "m_TexEnvs"] {
                    let mut values = saved[field].array().to_vec();
                    let names: std::collections::HashSet<_> = entries(&saved[field])
                        .map(|(name, _)| name.to_owned())
                        .collect();
                    for entry in shader.data["default_properties"][field].array() {
                        let name = if entry[0].as_str().is_some() {
                            entry[0].text()
                        } else {
                            entry[0]["name"].text()
                        };
                        if !names.contains(name) {
                            values.push(entry.clone());
                        }
                    }
                    saved.set(field, crate::value::array(values));
                }
            }
            let mut colors = serde_json::Map::new();
            let mut floats = serde_json::Map::new();
            let mut textures = serde_json::Map::new();
            for (name, value) in entries(&saved["m_Colors"]) {
                colors.insert(name.into(), properties(value, "rgba"));
            }
            for (name, value) in entries(&saved["m_Floats"]) {
                floats.insert(name.into(), properties(value, ""));
            }
            for (name, value) in entries(&saved["m_TexEnvs"]) {
                let reference = rawref(value, "m_Texture");
                let texture = reference.and_then(|id| self.texture(id, objects));
                let mut entry = json!({"texture":reference.and_then(|id|objects.get(&id)).map(|n|n.data["name"].text()),
                    "object":reference,"scale":properties(&value["m_Scale"],"xy"),"offset":properties(&value["m_Offset"],"xy")});
                if let Some(texture) = texture {
                    entry["index"] = texture.into();
                    if name == "_MainTex" {
                        material["pbrMetallicRoughness"]["baseColorTexture"] =
                            json!({"index":texture});
                    }
                }
                textures.insert(name.into(), entry);
            }
            if let Some(color) = colors.get("_Color") {
                material["pbrMetallicRoughness"]["baseColorFactor"] = json!(
                    color
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|c| c.as_f64().unwrap().clamp(0.0, 1.0))
                        .collect::<Vec<_>>()
                );
            }
            let lo = shader_name.to_ascii_lowercase();
            if lo.contains("test") || lo.contains("cutout") {
                material["alphaMode"] = "MASK".into();
                material["alphaCutoff"] = floats.get("_Cutoff").cloned().unwrap_or(json!(0.5));
            } else if lo.contains("blend")
                || lo.contains("transparent")
                || lo.contains("toonshading")
                || lo.starts_with("skin_")
                || lo == "ffwater"
                || lo == "ffpoison"
            {
                material["alphaMode"] = "BLEND".into();
            }
            if lo.contains("culloff") {
                material["doubleSided"] = true.into();
            }
            material["name"] = raw["m_Name"].text().into();
            material["extras"] = json!({"object":id,"shader":shader_name,"shader_object":rawref(raw,"m_Shader"),
                "colors":colors,"floats":floats,"textures":textures});
        }
        let index = self.add("materials", material);
        self.materials.insert(id, index);
        Ok(index)
    }
}
