use anyhow::{Result, bail};
use rmpv::Value;
use std::{fs, path::Path};

pub trait Fields {
    fn as_map_mut(&mut self) -> Option<&mut Vec<(Value, Value)>>;
    fn array(&self) -> &[Value];
    fn integer(&self) -> i64;
    fn number(&self) -> f64;
    fn text(&self) -> &str;
    fn binary(&self) -> &[u8];
    fn truth(&self) -> bool;
    fn id(&self) -> Option<u32>;
    fn set(&mut self, name: &str, value: Value);
    fn without(&self, names: &[&str]) -> Value;
}

impl Fields for Value {
    fn as_map_mut(&mut self) -> Option<&mut Vec<(Value, Value)>> {
        if let Value::Map(entries) = self {
            Some(entries)
        } else {
            None
        }
    }

    fn array(&self) -> &[Value] {
        self.as_array().map(Vec::as_slice).unwrap_or(&[])
    }

    fn integer(&self) -> i64 {
        self.as_i64()
            .or_else(|| self.as_u64().map(|n| n as i64))
            .unwrap_or(0)
    }

    fn number(&self) -> f64 {
        self.as_f64().unwrap_or_else(|| self.integer() as f64)
    }

    fn text(&self) -> &str {
        self.as_str().unwrap_or("")
    }

    fn binary(&self) -> &[u8] {
        self.as_slice().unwrap_or(&[])
    }

    fn truth(&self) -> bool {
        match self {
            Value::Nil => false,
            Value::Boolean(b) => *b,
            Value::Integer(_) => self.integer() != 0,
            Value::Array(a) => !a.is_empty(),
            _ => true,
        }
    }

    fn id(&self) -> Option<u32> {
        self.as_u64().and_then(|n| u32::try_from(n).ok())
    }

    fn set(&mut self, name: &str, value: Value) {
        let entries = self.as_map_mut().expect("descriptor must be a map");
        if let Some((_, item)) = entries
            .iter_mut()
            .find(|(key, _)| key.as_str() == Some(name))
        {
            *item = value;
        } else {
            entries.push((name.into(), value));
        }
    }

    fn without(&self, names: &[&str]) -> Value {
        Value::Map(
            self.as_map()
                .into_iter()
                .flatten()
                .filter(|(key, _)| !names.contains(&key.text()))
                .cloned()
                .collect(),
        )
    }
}

pub fn map<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Map(
        entries
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect(),
    )
}

pub fn array<T: Into<Value>>(values: impl IntoIterator<Item = T>) -> Value {
    Value::Array(values.into_iter().map(Into::into).collect())
}

pub fn serial(value: &impl serde::Serialize) -> Value {
    unpack(&rmp_serde::to_vec_named(value).expect("serializable metadata"))
        .expect("valid MessagePack")
}

pub fn packed(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    rmpv::encode::write_value(&mut bytes, value)?;
    Ok(bytes)
}

pub fn unpack(bytes: &[u8]) -> Result<Value> {
    Ok(rmpv::decode::read_value(&mut std::io::Cursor::new(bytes))?)
}

pub fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)?;
    Ok(())
}

pub fn write_pack(path: &Path, value: &Value) -> Result<()> {
    write(path, &packed(value)?)
}

pub fn json(value: &Value) -> Result<serde_json::Value> {
    use serde_json::{Map, Value as J};

    Ok(match value {
        Value::Nil => J::Null,
        Value::Boolean(b) => J::Bool(*b),
        Value::Integer(i) => {
            if let Some(n) = i.as_i64() {
                n.into()
            } else {
                i.as_u64().unwrap().into()
            }
        }
        Value::F32(f) => serde_json::Number::from_f64(*f as f64)
            .map(J::Number)
            .ok_or_else(|| anyhow::anyhow!("Nonfinite JSON float"))?,
        Value::F64(f) => serde_json::Number::from_f64(*f)
            .map(J::Number)
            .ok_or_else(|| anyhow::anyhow!("Nonfinite JSON float"))?,
        Value::String(s) => J::String(
            s.as_str()
                .ok_or_else(|| anyhow::anyhow!("Invalid UTF-8 metadata"))?
                .into(),
        ),
        Value::Array(a) => J::Array(a.iter().map(json).collect::<Result<_>>()?),
        Value::Map(m) => {
            let mut result = Map::new();
            for (k, v) in m {
                let key = if let Some(s) = k.as_str() {
                    s.to_owned()
                } else {
                    k.integer().to_string()
                };
                result.insert(key, json(v)?);
            }
            J::Object(result)
        }
        _ => bail!("Binary data cannot be written to JSON"),
    })
}

pub fn write_json(path: &Path, value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(&json(value)?)?;
    bytes.push(b'\n');
    write(path, &bytes)
}

pub fn content(root: &Path, folder: &str, bytes: &[u8], extension: &str) -> Result<String> {
    use sha2::{Digest, Sha256};

    let relative = format!("{folder}/{}{extension}", hex::encode(Sha256::digest(bytes)));
    let path = root.join(&relative);
    if !path.exists() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);

        let temporary = path.with_extension(format!(
            "part-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        write(&temporary, bytes)?;
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
    }

    Ok(relative)
}

pub fn reference(value: &Value) -> Option<u32> {
    value["r"].id()
}

pub fn rawref(value: &Value, name: &str) -> Option<u32> {
    reference(&value[name])
}

pub fn refs(value: &Value, output: &mut Vec<u32>) {
    match value {
        Value::Map(m) if m.len() == 1 && m[0].0.as_str() == Some("r") => {
            if let Some(id) = m[0].1.id() {
                output.push(id);
            }
        }
        Value::Map(m) => {
            for (k, v) in m {
                if !["m_Script", "m_DefaultPhysicMaterial"].contains(&k.text()) {
                    refs(v, output);
                }
            }
        }
        Value::Array(a) => {
            for v in a {
                refs(v, output);
            }
        }
        _ => {}
    }
}
