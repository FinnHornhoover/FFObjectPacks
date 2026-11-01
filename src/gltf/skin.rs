use crate::{geometry::packed_ints, value::Fields};
use anyhow::{Result, ensure};
use glam::{DMat4, DQuat, DVec3};
use rmpv::Value;

pub fn trs(data: &Value) -> DMat4 {
    let vector = |key: &str| {
        DVec3::new(
            data[key][0].number(),
            data[key][1].number(),
            data[key][2].number(),
        )
    };
    let q = DQuat::from_xyzw(
        data["q"][0].number(),
        data["q"][1].number(),
        data["q"][2].number(),
        data["q"][3].number(),
    );
    let q = if q.length_squared() > 0.0 {
        q.normalize()
    } else {
        DQuat::IDENTITY
    };
    DMat4::from_scale_rotation_translation(vector("s"), q, vector("p"))
}

pub fn mirror(matrix: DMat4) -> DMat4 {
    let reflect = DMat4::from_diagonal(glam::DVec4::new(-1.0, 1.0, 1.0, 1.0));
    reflect * matrix * reflect
}

pub fn bind(value: &Value) -> DMat4 {
    let mut data = [0.0; 16];
    for row in 0..4 {
        for col in 0..4 {
            data[col * 4 + row] = value[format!("e{row}{col}").as_str()].number();
        }
    }
    DMat4::from_cols_array(&data)
}

pub fn weights(raw: &Value, vertices: usize, bones: usize) -> Result<(Vec<u16>, Vec<f32>)> {
    let mut rows = Vec::new();
    if !raw["m_Skin"].array().is_empty() {
        for vertex in raw["m_Skin"].array() {
            let mut row = Vec::new();
            for i in 0..4 {
                let weight = vertex[format!("weight[{i}]").as_str()].number();
                if weight != 0.0 {
                    row.push((vertex[format!("boneIndex[{i}]").as_str()].integer(), weight));
                }
            }
            rows.push(row);
        }
    } else {
        let compressed = &raw["compressed_channels"];
        let weights = packed_ints(&compressed["m_Weights"])?;
        let indices = packed_ints(&compressed["m_BoneIndices"])?;
        let mut indices = indices.into_iter();
        let mut row = Vec::new();
        let mut total = 0u32;
        for weight in weights {
            ensure!(
                weight <= 31 && total + weight <= 31,
                "Invalid packed skin weight"
            );
            let bone = indices
                .next()
                .ok_or_else(|| anyhow::anyhow!("Missing packed bone index"))?;
            row.push((i64::from(bone), f64::from(weight) / 31.0));
            total += weight;
            if total < 31 && row.len() == 3 {
                let bone = indices
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("Missing implicit bone index"))?;
                row.push((i64::from(bone), f64::from(31 - total) / 31.0));
                total = 31;
            }
            if total == 31 {
                rows.push(std::mem::take(&mut row));
                total = 0;
            }
        }
        ensure!(
            row.is_empty() && indices.next().is_none(),
            "Packed skin channels end differently"
        );
    }
    ensure!(rows.len() == vertices, "Skin vertex count differs");
    let mut joints = Vec::with_capacity(vertices * 4);
    let mut weights = Vec::with_capacity(vertices * 4);
    for row in rows {
        let mut merged = std::collections::BTreeMap::<i64, f64>::new();
        for (bone, weight) in row {
            ensure!(weight.is_finite() && weight >= 0.0, "Invalid skin weight");
            if weight == 0.0 {
                continue;
            }
            ensure!(
                bone >= 0 && (bone as usize) < bones,
                "Skin bone index exceeds renderer bones"
            );
            *merged.entry(bone).or_default() += weight;
        }
        let mut row: Vec<_> = merged.into_iter().collect();
        row.sort_by(|a, b| b.1.total_cmp(&a.1));
        row.truncate(4);
        let sum = row.iter().map(|(_, w)| w).sum::<f64>();
        ensure!(sum > 0.0, "Skin vertex has no positive weight");
        row.resize(4, (0, 0.0));
        for (bone, weight) in row {
            joints.push(u16::try_from(bone)?);
            weights.push((weight / sum) as f32);
        }
    }
    Ok((joints, weights))
}
