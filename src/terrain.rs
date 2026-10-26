use crate::value::{Fields, array, map, packed};
use anyhow::{Result, ensure};
use flate2::{Compression, write::ZlibEncoder};
use rmpv::Value;
use std::{collections::HashMap, io::Write};

fn compressed_size(data: &[u8]) -> Result<usize> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(data)?;
    Ok(encoder.finish()?.len())
}

pub fn heights(raw: &Value) -> Result<Vec<u8>> {
    let width = usize::try_from(raw["m_Width"].integer())?;
    let height = usize::try_from(raw["m_Height"].integer())?;
    let values = raw["m_Heights"].array();
    ensure!(
        width.checked_mul(height) == Some(values.len()),
        "Invalid heightfield dimensions"
    );

    let signed = values.iter().any(|v| v.integer() < 0);
    let mut original = Vec::with_capacity(values.len());
    for value in values {
        let number = value.integer();
        ensure!(
            if signed {
                (-32768..=32767).contains(&number)
            } else {
                (0..=65535).contains(&number)
            },
            "Invalid 16-bit height sample"
        );
        original.push(number as u16);
    }

    let mut choices = Vec::new();
    for magic in [b"HMP1", b"HMD1", b"HMX1"] {
        let mut data = magic.to_vec();
        data.extend((width as u32).to_le_bytes());
        data.extend((height as u32).to_le_bytes());
        data.extend(u32::from(signed).to_le_bytes());
        for (i, value) in original.iter().enumerate() {
            let previous = if i % width == 0 { 0 } else { original[i - 1] };
            let encoded = match magic {
                b"HMD1" => value.wrapping_sub(previous),
                b"HMX1" => value ^ previous,
                _ => *value,
            };
            data.extend(encoded.to_le_bytes());
        }
        choices.push((compressed_size(&data)?, data));
    }

    Ok(choices.into_iter().min_by_key(|(size, _)| *size).unwrap().1)
}

fn bytes(value: &Value) -> Result<Vec<u8>> {
    if let Value::Binary(data) = value {
        return Ok(data.clone());
    }
    value
        .array()
        .iter()
        .map(|v| Ok(u8::try_from(v.integer())?))
        .collect()
}

pub fn details(patches: &Value) -> Result<Vec<u8>> {
    let mut records = Vec::new();
    let mut lengths = Vec::new();
    for patch in patches.array() {
        let mut data = Vec::new();
        for key in ["m_Center", "m_Extent"] {
            for axis in ["x", "y", "z"] {
                data.extend((patch["bounds"][key][axis].number() as f32).to_le_bytes());
            }
        }

        let layers = bytes(&patch["layerIndices"])?;
        let counts = bytes(&patch["numberOfObjects"])?;
        lengths.push((layers.len(), counts.len()));
        data.extend((layers.len() as u32).to_le_bytes());
        data.extend((counts.len() as u32).to_le_bytes());
        data.extend(layers);
        data.extend(counts);
        records.push(data);
    }

    if records.is_empty() || lengths.iter().any(|n| *n != lengths[0]) {
        let mut palette = Vec::new();
        let mut lookup = HashMap::new();
        let mut indices = Vec::new();
        for record in records {
            let next = palette.len();
            let index = *lookup.entry(record.clone()).or_insert_with(|| {
                palette.push(Value::Binary(record));
                next
            });
            indices.push(index as u64);
        }
        return packed(&map([
            ("mode", "local-palette".into()),
            ("records", array(palette)),
            ("indices", array(indices)),
        ]));
    }

    let (layer_bytes, count_bytes) = lengths[0];
    let counts: Vec<u8> = records
        .iter()
        .flat_map(|r| r[32 + layer_bytes..].iter().copied())
        .collect();

    let mut sparse = Vec::new();
    let mut previous = -1i64;
    for (i, n) in counts.iter().enumerate() {
        if *n == 0 {
            continue;
        }

        let mut gap = i as i64 - previous;
        previous = i as i64;
        while gap >= 128 {
            sparse.push((gap as u8 & 127) | 128);
            gap >>= 7;
        }
        sparse.push(gap as u8);
        sparse.push(*n);
    }

    let use_sparse = compressed_size(&sparse)? < compressed_size(&counts)?;

    packed(&map([
        ("mode", "sparse-soa".into()),
        ("patches", (records.len() as u64).into()),
        ("layer_bytes", (layer_bytes as u64).into()),
        ("count_bytes", (count_bytes as u64).into()),
        (
            "bounds",
            Value::Binary(
                records
                    .iter()
                    .flat_map(|r| r[..24].iter().copied())
                    .collect(),
            ),
        ),
        (
            "layer_indices",
            Value::Binary(
                records
                    .iter()
                    .flat_map(|r| r[32..32 + layer_bytes].iter().copied())
                    .collect(),
            ),
        ),
        (
            "count_encoding",
            if use_sparse { "delta-nonzero" } else { "dense" }.into(),
        ),
        ("count_length", (counts.len() as u64).into()),
        (
            "counts",
            Value::Binary(if use_sparse { sparse } else { counts }),
        ),
    ]))
}
