use crate::value::{Fields, array, map, packed};
use anyhow::{Result, ensure};
use rmpv::Value;
use serde_json::{Value as Json, json};

pub struct Mesh {
    pub glb: Vec<u8>,
    pub skin: Option<Vec<u8>>,
    pub info: Value,
}

fn packed_ints(raw: &Value) -> Result<Vec<u32>> {
    let count = usize::try_from(raw["m_NumItems"].integer())?;
    let bits = usize::try_from(raw["m_BitSize"].integer())?;
    ensure!(bits <= 32, "Packed integer exceeds 32 bits");
    if bits == 0 {
        return Ok(vec![0; count]);
    }

    let bytes = raw["m_Data"].binary();
    ensure!(
        count
            .checked_mul(bits)
            .is_some_and(|n| n <= bytes.len() * 8),
        "Truncated packed channel"
    );

    let mut output = Vec::with_capacity(count);
    let mask = (1u64 << bits) - 1;
    for i in 0..count {
        let offset = i * bits;
        let start = offset / 8;
        let end = (offset + bits).div_ceil(8);
        let mut number = 0u64;
        for (j, byte) in bytes[start..end].iter().enumerate() {
            number |= u64::from(*byte) << (j * 8);
        }
        output.push(((number >> (offset % 8)) & mask) as u32);
    }

    Ok(output)
}

fn packed_floats(raw: &Value) -> Result<Vec<f64>> {
    let bits = raw["m_BitSize"].integer();
    let scale = if bits == 0 {
        0.0
    } else {
        raw["m_Range"].number() / ((1u64 << bits) - 1) as f64
    };
    let start = raw["m_Start"].number();

    Ok(packed_ints(raw)?
        .into_iter()
        .map(|n| n as f64 * scale + start)
        .collect())
}

fn vectors(raw: &Value, axes: &str) -> Vec<Vec<f32>> {
    raw.array()
        .iter()
        .map(|v| {
            axes.chars()
                .map(|c| v[c.to_string().as_str()].number() as f32)
                .collect()
        })
        .collect()
}

fn chunks(values: Vec<f64>, width: usize) -> Result<Vec<Vec<f32>>> {
    ensure!(
        values.len().is_multiple_of(width),
        "Partial compressed vertex channel"
    );
    Ok(values
        .chunks(width)
        .map(|c| c.iter().map(|n| *n as f32).collect())
        .collect())
}

enum Format {
    Float,
    Color,
    Indices16,
    Indices32,
}

impl Format {
    fn settings(self) -> (u32, u32, bool) {
        match self {
            Self::Float => (5126, 34962, false),
            Self::Color => (5121, 34962, true),
            Self::Indices16 => (5123, 34963, false),
            Self::Indices32 => (5125, 34963, false),
        }
    }
}

struct Glb {
    binary: Vec<u8>,
    views: Vec<Json>,
    accessors: Vec<Json>,
}

impl Glb {
    fn add(
        &mut self,
        bytes: &[u8],
        count: usize,
        kind: &str,
        format: Format,
        bounds: Option<(Vec<f64>, Vec<f64>)>,
    ) -> usize {
        let (component, target, normalized) = format.settings();
        let index = self.accessors.len();
        self.views.push(json!({
            "buffer": 0,
            "byteOffset": self.binary.len(),
            "byteLength": bytes.len(),
            "target": target
        }));

        self.binary.extend(bytes);
        while !self.binary.len().is_multiple_of(4) {
            self.binary.push(0);
        }

        let mut accessor = json!({
            "bufferView": self.views.len() - 1,
            "componentType": component,
            "count": count,
            "type": kind
        });
        if normalized {
            accessor["normalized"] = true.into();
        }
        if let Some((min, max)) = bounds {
            accessor["min"] = json!(min);
            accessor["max"] = json!(max);
        }

        self.accessors.push(accessor);
        index
    }

    fn floats(&mut self, values: &[Vec<f32>], kind: &str, bounds: bool) -> usize {
        let bytes: Vec<u8> = values
            .iter()
            .flatten()
            .flat_map(|n| n.to_le_bytes())
            .collect();

        let bounds = if bounds {
            let width = values[0].len();
            let mut min = vec![f64::INFINITY; width];
            let mut max = vec![f64::NEG_INFINITY; width];
            for v in values {
                for (i, n) in v.iter().enumerate() {
                    min[i] = min[i].min(*n as f64);
                    max[i] = max[i].max(*n as f64);
                }
            }
            Some((min, max))
        } else {
            None
        };

        self.add(&bytes, values.len(), kind, Format::Float, bounds)
    }
}

fn glb(document: &Json, binary: &[u8]) -> Result<Vec<u8>> {
    let mut json = serde_json::to_vec(document)?;
    while json.len() % 4 != 0 {
        json.push(b' ');
    }

    let mut chunks = Vec::new();
    chunks.extend((json.len() as u32).to_le_bytes());
    chunks.extend(b"JSON");
    chunks.extend(json);

    if !binary.is_empty() {
        let length = binary.len().next_multiple_of(4);
        chunks.extend((length as u32).to_le_bytes());
        chunks.extend(b"BIN\0");
        chunks.extend(binary);
        chunks.resize(chunks.len() + length - binary.len(), 0);
    }

    let mut result = b"glTF".to_vec();
    result.extend(2u32.to_le_bytes());
    result.extend((12u32 + chunks.len() as u32).to_le_bytes());
    result.extend(chunks);
    Ok(result)
}

pub fn convert(raw: &Value) -> Result<Mesh> {
    let compressed = raw["m_MeshCompression"].truth();
    let channels = &raw["m_CompressedMesh"];
    let (mut positions, mut normals, uv, packed_indices) = if compressed {
        let positions = chunks(packed_floats(&channels["m_Vertices"])?, 3)?;

        let xy = packed_floats(&channels["m_Normals"])?;
        let signs = packed_ints(&channels["m_NormalSigns"])?;
        ensure!(
            xy.len() == signs.len() * 2,
            "Compressed normal sign count differs"
        );

        let normals = xy
            .chunks(2)
            .zip(signs)
            .map(|(v, sign)| {
                vec![
                    v[0] as f32,
                    v[1] as f32,
                    ((1.0 - (v[0] * v[0] + v[1] * v[1])).max(0.0).sqrt()
                        * if sign != 0 { 1.0 } else { -1.0 }) as f32,
                ]
            })
            .collect();
        (
            positions,
            normals,
            chunks(packed_floats(&channels["m_UV"])?, 2)?,
            Some(packed_ints(&channels["m_Triangles"])?),
        )
    } else {
        (
            vectors(&raw["m_Vertices"], "xyz"),
            vectors(&raw["m_Normals"], "xyz"),
            vectors(&raw["m_UV"], "xy"),
            None,
        )
    };

    let count = positions.len();
    let mut document = json!({
        "asset": { "version": "2.0", "generator": "FFObjectPacks" },
        "scene": 0,
        "scenes": [{ "nodes": [] }]
    });
    if count == 0 {
        return Ok(Mesh {
            glb: glb(&document, &[])?,
            skin: None,
            info: map([
                ("vertices", 0.into()),
                ("triangles", 0.into()),
                ("attributes", array(Vec::<Value>::new())),
                ("source_empty", true.into()),
            ]),
        });
    }

    ensure!(
        positions.iter().flatten().all(|n| n.is_finite()),
        "Nonfinite vertex position"
    );
    for position in &mut positions {
        position[0] *= -1.0;
    }
    for normal in &mut normals {
        normal[0] *= -1.0;
    }

    let mut writer = Glb {
        binary: Vec::new(),
        views: Vec::new(),
        accessors: Vec::new(),
    };
    let mut attributes = serde_json::Map::new();
    attributes.insert(
        "POSITION".into(),
        writer.floats(&positions, "VEC3", true).into(),
    );
    if !normals.is_empty() {
        ensure!(
            normals.len() == count,
            "Normal count differs from vertex count"
        );
        attributes.insert(
            "NORMAL".into(),
            writer.floats(&normals, "VEC3", false).into(),
        );
    }

    ensure!(
        uv.len() % count == 0,
        "UV channel has a partial vertex array"
    );
    for (index, values) in uv.chunks(count).enumerate() {
        attributes.insert(
            format!("TEXCOORD_{index}"),
            writer.floats(values, "VEC2", false).into(),
        );
    }
    if !compressed {
        for (index, name) in ["m_UV1", "m_UV2", "m_UV3"].iter().enumerate() {
            let values = vectors(&raw[*name], "xy");
            if !values.is_empty() {
                ensure!(values.len() == count, "UV count differs from vertex count");
                attributes.insert(
                    format!("TEXCOORD_{}", index + 1),
                    writer.floats(&values, "VEC2", false).into(),
                );
            }
        }
    }

    let colors = raw["m_Colors"].array();
    if !colors.is_empty() {
        ensure!(
            colors.len() == count,
            "Colour count differs from vertex count"
        );
        let integers = colors.iter().all(|v| {
            !v["rgba"].is_nil()
                || ["r", "g", "b", "a"]
                    .iter()
                    .all(|k| v[*k].is_i64() || v[*k].is_u64())
        });
        let values: Vec<Vec<f32>> = colors
            .iter()
            .map(|v| {
                if !v["rgba"].is_nil() {
                    (0..4)
                        .map(|i| ((v["rgba"].integer() >> (8 * i)) & 255) as f32)
                        .collect()
                } else {
                    ["r", "g", "b", "a"]
                        .iter()
                        .map(|k| v[*k].number() as f32)
                        .collect()
                }
            })
            .collect();
        let index = if integers {
            let bytes: Vec<u8> = values.iter().flatten().map(|n| *n as u8).collect();
            writer.add(&bytes, count, "VEC4", Format::Color, None)
        } else {
            writer.floats(&values, "VEC4", false)
        };
        attributes.insert("COLOR_0".into(), index.into());
    }

    let mut tangents = vectors(&raw["m_Tangents"], "xyzw");
    if !tangents.is_empty() {
        ensure!(
            tangents.len() == count,
            "Tangent count differs from vertex count"
        );
        for tangent in &mut tangents {
            tangent[0] *= -1.0;
            tangent[3] *= -1.0;
        }
        attributes.insert(
            "TANGENT".into(),
            writer.floats(&tangents, "VEC4", false).into(),
        );
    }

    let mut primitives = Vec::new();
    let mut triangle_count = 0;
    let submeshes = raw["m_SubMeshes"].array();
    for (index, submesh) in submeshes.iter().enumerate() {
        let offset = usize::try_from(submesh["firstByte"].integer())?;
        let length = usize::try_from(submesh["indexCount"].integer())?;
        let indices: Vec<u32> = if let Some(packed) = &packed_indices {
            ensure!(
                offset / 2 + length <= packed.len(),
                "Submesh index range exceeds packed buffer"
            );
            packed[offset / 2..offset / 2 + length].to_vec()
        } else {
            let stride = if raw["m_Use16BitIndices"].is_nil() || raw["m_Use16BitIndices"].truth() {
                2
            } else {
                4
            };
            let bytes = raw["m_IndexBuffer"].binary();
            let end = offset
                .checked_add(
                    length
                        .checked_mul(stride)
                        .ok_or_else(|| anyhow::anyhow!("Index count overflow"))?,
                )
                .ok_or_else(|| anyhow::anyhow!("Index offset overflow"))?;
            ensure!(end <= bytes.len(), "Submesh index range exceeds buffer");
            bytes[offset..end]
                .chunks(stride)
                .map(|b| {
                    if stride == 2 {
                        u16::from_le_bytes(b.try_into().unwrap()) as u32
                    } else {
                        u32::from_le_bytes(b.try_into().unwrap())
                    }
                })
                .collect()
        };

        let mut faces = Vec::new();
        if submesh["isTriStrip"].truth() {
            for i in 2..indices.len() {
                let a = indices[i - 2];
                let b = indices[i - 1];
                let c = indices[i];
                if a != b && a != c && b != c {
                    faces.extend(if i % 2 == 0 { [a, b, c] } else { [c, b, a] });
                }
            }
        } else {
            ensure!(
                indices.len().is_multiple_of(3),
                "Triangle list has a partial triangle"
            );
            faces = indices;
        }

        if faces.is_empty() {
            continue;
        }
        ensure!(
            faces.iter().all(|id| (*id as usize) < count),
            "Vertex index out of bounds"
        );
        for face in faces.chunks_mut(3) {
            face.reverse();
        }

        let short = count < 65536;
        let format = if short {
            Format::Indices16
        } else {
            Format::Indices32
        };
        let bytes: Vec<u8> = faces
            .iter()
            .flat_map(|n| {
                if short {
                    (*n as u16).to_le_bytes().to_vec()
                } else {
                    n.to_le_bytes().to_vec()
                }
            })
            .collect();
        let accessor = writer.add(&bytes, faces.len(), "SCALAR", format, None);
        primitives.push(json!({
            "attributes": attributes.clone(),
            "indices": accessor,
            "material": index
        }));
        triangle_count += faces.len() / 3;
    }

    ensure!(
        !primitives.is_empty(),
        "Nonempty mesh contains no triangles"
    );
    document["scenes"] = json!([{ "nodes": [0] }]);
    document["nodes"] = json!([{ "mesh": 0 }]);
    document["meshes"] = json!([{ "primitives": primitives }]);
    document["materials"] = json!(
        (0..submeshes.len())
            .map(|i| json!({ "name": format!("slot_{i}") }))
            .collect::<Vec<_>>()
    );
    document["buffers"] = json!([{ "byteLength": writer.binary.len() }]);
    document["bufferViews"] = json!(writer.views);
    document["accessors"] = json!(writer.accessors);

    let mut auxiliary = map([]);
    for name in ["m_Skin", "m_BindPose"] {
        if !raw[name].array().is_empty() {
            auxiliary.set(name, raw[name].clone());
        }
    }
    let remaining = Value::Map(
        channels
            .as_map()
            .into_iter()
            .flatten()
            .filter(|(name, value)| {
                ![
                    "m_Vertices",
                    "m_UV",
                    "m_Triangles",
                    "m_Normals",
                    "m_NormalSigns",
                ]
                .contains(&name.text())
                    && value["m_NumItems"].integer() != 0
            })
            .cloned()
            .collect(),
    );
    if !remaining.as_map().unwrap().is_empty() {
        auxiliary.set("compressed_channels", remaining);
    }

    let skin = if auxiliary.as_map().unwrap().is_empty() {
        None
    } else {
        Some(packed(&auxiliary)?)
    };

    Ok(Mesh {
        glb: glb(&document, &writer.binary)?,
        skin,
        info: map([
            ("vertices", (count as u64).into()),
            ("triangles", (triangle_count as u64).into()),
            ("attributes", array(attributes.keys().cloned())),
            ("source_empty", false.into()),
        ]),
    })
}
