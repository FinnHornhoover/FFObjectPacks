use super::reader::Reader;
use anyhow::{Result, ensure};
use rmpv::Value;
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct Tree {
    pub kind: String,
    pub name: String,
    pub size: i32,
    pub array: bool,
    pub aligned: bool,
    pub children: Vec<Tree>,
    pub fixed: usize,
}

impl Tree {
    pub fn read(r: &mut Reader<'_>, depth: usize) -> Result<Self> {
        ensure!(depth < 128, "Type tree nesting exceeds 128");

        let kind = r.cstr()?;
        let name = r.cstr()?;
        let size = r.i32()?;
        r.i32()?;
        let array = r.i32()? != 0;
        r.i32()?;
        let aligned = r.i32()? & 0x4000 != 0;

        let count = r.count()?;
        let mut children = Vec::with_capacity(count);
        for _ in 0..count {
            children.push(Self::read(r, depth + 1)?);
        }

        let fixed = match kind.as_str() {
            "bool" | "SInt8" | "UInt8" => 1,
            "SInt16" | "UInt16" => 2,
            "int" | "SInt32" | "UInt32" | "unsigned int" | "float" => 4,
            "SInt64" | "UInt64" | "double" => 8,
            s if s.starts_with("PPtr<") => 8,
            "string" => 0,
            _ if array || children.first().is_some_and(|c| c.array) => 0,
            _ if !children.is_empty() && children.iter().all(|c| c.fixed > 0 && !c.aligned) => {
                children.iter().map(|c| c.fixed).sum()
            }
            _ => 0,
        };

        Ok(Self {
            kind,
            name,
            size,
            array,
            aligned,
            children,
            fixed,
        })
    }

    pub fn decode(
        &self,
        r: &mut Reader<'_>,
        format: u32,
        keep: bool,
        fields: Option<&HashSet<&str>>,
    ) -> Result<Value> {
        let start = r.pos;
        let mut extra_align = false;

        let value = if !keep && self.fixed > 0 {
            r.take(self.fixed)?;
            Value::Nil
        } else {
            match self.kind.as_str() {
                "bool" => Value::Boolean(r.u8()? != 0),
                "SInt8" => (r.u8()? as i8).into(),
                "UInt8" => r.u8()?.into(),
                "SInt16" => (r.uint(2)? as i16).into(),
                "UInt16" => (r.uint(2)? as u16).into(),
                "int" | "SInt32" => r.i32()?.into(),
                "UInt32" | "unsigned int" => r.u32()?.into(),
                "SInt64" | "UInt64" => (r.uint(8)? as i64).into(),
                "float" => Value::F64(f32::from_bits(r.u32()?) as f64),
                "double" => Value::F64(f64::from_bits(r.uint(8)?)),
                "string" => {
                    let count = r.count()?;
                    let data = r.take(count)?;
                    extra_align = self.children.first().is_some_and(|c| c.aligned);

                    if keep {
                        match std::str::from_utf8(data) {
                            Ok(s) => s.into(),
                            Err(_) => Value::Binary(data.to_vec()),
                        }
                    } else {
                        Value::Nil
                    }
                }
                s if s.starts_with("PPtr<") => {
                    let first = r.u32()?;
                    let second = r.u32()?;
                    let (file, path) = if format == 7 {
                        (
                            (first & 65535) as i64,
                            (((first as u64 >> 16) << 32) | second as u64) as i64,
                        )
                    } else {
                        (first as i32 as i64, second as i32 as i64)
                    };

                    if keep && (file != 0 || path != 0) {
                        let mut bytes = Vec::with_capacity(16);
                        bytes.extend(file.to_le_bytes());
                        bytes.extend(path.to_le_bytes());
                        Value::Ext(-1, bytes)
                    } else {
                        Value::Nil
                    }
                }
                _ => {
                    let holder = if self.array {
                        Some(self)
                    } else {
                        self.children.first().filter(|c| c.array)
                    };

                    if let Some(holder) = holder {
                        extra_align = holder.aligned;
                        ensure!(holder.children.len() >= 2, "Invalid array type tree");

                        let item = &holder.children[1];
                        let count = r.count()?;

                        if item.kind == "char" || item.kind == "UInt8" {
                            let bytes = r.take(count)?;
                            if keep {
                                Value::Binary(bytes.to_vec())
                            } else {
                                Value::Nil
                            }
                        } else if !keep && item.fixed > 0 && !item.aligned {
                            r.take(
                                count
                                    .checked_mul(item.fixed)
                                    .ok_or_else(|| anyhow::anyhow!("Array size overflow"))?,
                            )?;
                            Value::Nil
                        } else {
                            let mut values = if keep {
                                Vec::with_capacity(count)
                            } else {
                                Vec::new()
                            };

                            for _ in 0..count {
                                let value = item.decode(r, format, keep, None)?;
                                if keep {
                                    values.push(value);
                                }
                            }
                            Value::Array(values)
                        }
                    } else if self.kind == "pair" {
                        let mut values = Vec::with_capacity(2);
                        for child in &self.children {
                            let v = child.decode(r, format, keep, None)?;
                            if keep {
                                values.push(v);
                            }
                        }
                        Value::Array(values)
                    } else {
                        let mut values =
                            Vec::with_capacity(if keep { self.children.len() } else { 0 });
                        for child in &self.children {
                            let wanted =
                                keep && fields.is_none_or(|s| s.contains(child.name.as_str()));
                            let v = child.decode(r, format, wanted, None)?;
                            if wanted {
                                values.push((child.name.clone().into(), v));
                            }
                        }
                        Value::Map(values)
                    }
                }
            }
        };

        ensure!(
            self.size <= 0 || r.pos - start >= self.size as usize,
            "Object is shorter than its type tree ({})",
            self.name
        );

        if self.aligned || extra_align {
            r.pos = (r.pos + 3) & !3;
        }

        Ok(if keep { value } else { Value::Nil })
    }
}
