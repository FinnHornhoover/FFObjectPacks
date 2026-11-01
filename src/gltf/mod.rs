pub mod curves;
pub mod materials;
pub mod reduce;
pub mod skin;

use crate::{geometry, value::content};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{collections::HashMap, fs, path::Path};

pub struct Document {
    pub json: Value,
    binary: Vec<u8>,
    buffers: HashMap<String, usize>,
    pub materials: HashMap<Option<u32>, usize>,
    pub textures: HashMap<u32, usize>,
}

impl Document {
    pub fn new() -> Self {
        Self {
            json: json!({"asset":{"version":"2.0","generator":"FFObjectPacks"},
                "scene":0,"scenes":[{"nodes":[]}]}),
            binary: Vec::new(),
            buffers: HashMap::new(),
            materials: HashMap::new(),
            textures: HashMap::new(),
        }
    }

    pub fn add(&mut self, key: &str, value: Value) -> usize {
        let list = self
            .json
            .as_object_mut()
            .unwrap()
            .entry(key)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap();
        let index = list.len();
        list.push(value);
        index
    }

    pub fn buffer(&mut self, uri: &str, length: u64) -> usize {
        if let Some(index) = self.buffers.get(uri) {
            return *index;
        }
        let index = self.add("buffers", json!({"uri":uri,"byteLength":length}));
        self.buffers.insert(uri.into(), index);
        index
    }

    pub fn accessor(&mut self, values: &[f32], width: usize, bounds: bool) -> Result<usize> {
        ensure!(
            !values.is_empty() && values.len().is_multiple_of(width),
            "Invalid glTF accessor dimensions"
        );
        ensure!(
            values.iter().all(|v| v.is_finite()),
            "Nonfinite glTF accessor"
        );
        let offset = self.binary.len();
        for value in values {
            self.binary.extend(value.to_le_bytes());
        }
        let view = self.add(
            "bufferViews",
            json!({"buffer":u32::MAX,"byteOffset":offset,"byteLength":values.len()*4}),
        );
        let kind = match width {
            1 => "SCALAR",
            2 => "VEC2",
            3 => "VEC3",
            4 => "VEC4",
            16 => "MAT4",
            _ => unreachable!(),
        };
        let mut accessor =
            json!({"bufferView":view,"componentType":5126,"count":values.len()/width,"type":kind});
        if bounds {
            let mut min = vec![f32::INFINITY; width];
            let mut max = vec![f32::NEG_INFINITY; width];
            for row in values.chunks_exact(width) {
                for i in 0..width {
                    min[i] = min[i].min(row[i]);
                    max[i] = max[i].max(row[i]);
                }
            }
            accessor["min"] = json!(min);
            accessor["max"] = json!(max);
        }
        Ok(self.add("accessors", accessor))
    }

    pub fn joints(&mut self, values: &[u16]) -> usize {
        let offset = self.binary.len();
        for value in values {
            self.binary.extend(value.to_le_bytes());
        }
        let view=self.add("bufferViews",json!({"buffer":u32::MAX,"byteOffset":offset,"byteLength":values.len()*2,"target":34962}));
        while !self.binary.len().is_multiple_of(4) {
            self.binary.push(0);
        }
        self.add(
            "accessors",
            json!({"bufferView":view,"componentType":5123,"count":values.len()/4,"type":"VEC4"}),
        )
    }

    pub fn geometry(&mut self, source: &Value) -> Result<Vec<Value>> {
        let mut buffers = Vec::new();
        for buffer in source["buffers"].as_array().into_iter().flatten() {
            let uri = buffer["uri"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Geometry has embedded buffer"))?;
            buffers.push(self.buffer(uri, buffer["byteLength"].as_u64().unwrap()));
        }
        let mut views = Vec::new();
        for view in source["bufferViews"].as_array().into_iter().flatten() {
            let mut view = view.clone();
            view["buffer"] = buffers[view["buffer"].as_u64().unwrap() as usize].into();
            views.push(self.add("bufferViews", view));
        }
        let mut accessors = Vec::new();
        for accessor in source["accessors"].as_array().into_iter().flatten() {
            let mut accessor = accessor.clone();
            accessor["bufferView"] =
                views[accessor["bufferView"].as_u64().unwrap() as usize].into();
            accessors.push(self.add("accessors", accessor));
        }
        let mut primitives = Vec::new();
        for primitive in source["meshes"][0]["primitives"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let mut primitive = primitive.clone();
            for accessor in primitive["attributes"]
                .as_object_mut()
                .unwrap()
                .values_mut()
            {
                *accessor = accessors[accessor.as_u64().unwrap() as usize].into();
            }
            if let Some(index) = primitive["indices"].as_u64() {
                primitive["indices"] = accessors[index as usize].into();
            }
            primitives.push(primitive);
        }
        Ok(primitives)
    }

    pub fn save(mut self, root: &Path, folder: &str) -> Result<String> {
        if !self.binary.is_empty() {
            let file = content(root, "models/buffers", &self.binary, ".bin")?;
            let uri = relative(folder, &file);
            let buffer = self.buffer(&uri, self.binary.len() as u64);
            for view in self.json["bufferViews"].as_array_mut().unwrap() {
                if view["buffer"].as_u64() == Some(u32::MAX.into()) {
                    view["buffer"] = buffer.into();
                }
            }
        }
        content(root, folder, &geometry::glb(&self.json, &[])?, ".glb")
    }
}

pub fn relative(folder: &str, file: &str) -> String {
    let from: Vec<_> = folder.split('/').collect();
    let to: Vec<_> = file.split('/').collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    format!(
        "{}{}",
        "../".repeat(from.len() - common),
        to[common..].join("/")
    )
}

pub fn read(path: &Path) -> Result<Value> {
    let bytes = fs::read(path)?;
    ensure!(bytes.len() >= 20 && &bytes[..4] == b"glTF", "Invalid GLB");
    let size = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    ensure!(size <= bytes.len() - 20, "Truncated GLB JSON");
    Ok(serde_json::from_slice(&bytes[20..20 + size])?)
}

pub fn externalize(root: &Path, bytes: &[u8]) -> Result<(Vec<u8>, Option<String>)> {
    ensure!(bytes.len() >= 20, "Truncated GLB");
    let size = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let mut document: Value = serde_json::from_slice(&bytes[20..20 + size])?;
    let offset = 20 + size;
    let file = if bytes.len() > offset {
        ensure!(bytes.len() >= offset + 8, "Truncated GLB buffer");
        let length = document["buffers"][0]["byteLength"].as_u64().unwrap() as usize;
        ensure!(
            length <= bytes.len() - offset - 8,
            "Truncated geometry buffer"
        );
        let file = content(
            root,
            "models/buffers",
            &bytes[offset + 8..offset + 8 + length],
            ".bin",
        )?;
        document["buffers"][0]["uri"] = relative("models/geometry", &file).into();
        Some(file)
    } else {
        None
    };
    Ok((geometry::glb(&document, &[])?, file))
}
