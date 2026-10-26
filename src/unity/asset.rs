use super::{bundle::Member, reader::Reader, tree::Tree};
use anyhow::{Context, Result, ensure};
use indexmap::IndexMap;
use rmpv::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

pub type Trees = HashMap<i32, Arc<Tree>>;
pub type Fallback = HashMap<u32, Trees>;
#[derive(Clone)]
pub struct Object {
    pub path: i64,
    pub kind: i32,
    pub class: i32,
    pub offset: usize,
    pub size: usize,
}

pub struct Asset {
    pub name: String,
    pub version: String,
    pub format: u32,
    pub refs: Vec<String>,
    pub trees: Trees,
    pub objects: IndexMap<i64, Object>,
    pub data: Vec<u8>,
}

impl Asset {
    pub fn read(member: Member) -> Result<Self> {
        let mut r = Reader::new(&member.data, true);
        let metadata = r.u32()? as usize;
        let file_size = r.u32()? as usize;
        let format = r.u32()?;
        let data_offset = r.u32()? as usize;
        ensure!(
            format == 6 || format == 7,
            "Unsupported asset format {format} in {}",
            member.name
        );
        ensure!(
            file_size <= member.data.len() && metadata < file_size,
            "Invalid asset size in {}",
            member.name
        );

        r.big = false;
        r.seek(file_size - metadata + 1)?;
        let mut trees = Trees::new();
        let count = r.count()?;
        for _ in 0..count {
            let id = r.i32()?;
            trees.insert(id, Arc::new(Tree::read(&mut r, 0)?));
        }

        let long_ids = format == 7 && r.u32()? != 0;
        let count = r.count()?;
        let mut objects = IndexMap::with_capacity(count);
        for _ in 0..count {
            let path = if long_ids {
                r.uint(8)? as i64
            } else {
                r.i32()? as i64
            };
            let offset = (r.u32()? as usize)
                .checked_add(data_offset)
                .context("Object offset overflow")?;
            let size = r.u32()? as usize;
            let kind = r.i32()?;
            let class = r.uint(2)? as i16 as i32;
            r.uint(2)?;
            ensure!(
                offset
                    .checked_add(size)
                    .is_some_and(|end| end <= member.data.len()),
                "Object {path} exceeds asset buffer"
            );
            ensure!(
                objects
                    .insert(
                        path,
                        Object {
                            path,
                            kind,
                            class,
                            offset,
                            size
                        }
                    )
                    .is_none(),
                "Duplicate object {path}"
            );
        }

        let count = r.count()?;
        let mut refs = vec![member.name.clone()];
        for _ in 0..count {
            r.cstr()?;
            r.take(16)?;
            r.i32()?;
            refs.push(r.cstr()?);
        }

        ensure!(r.cstr()?.is_empty(), "Unexpected asset metadata suffix");

        Ok(Self {
            name: member.name,
            version: member.version,
            format,
            refs,
            trees,
            objects,
            data: member.data,
        })
    }

    pub fn schema<'a>(&'a self, obj: &Object, fallback: &'a Fallback) -> Option<&'a Tree> {
        self.trees
            .get(&obj.kind)
            .or_else(|| self.trees.get(&obj.class))
            .or_else(|| {
                if obj.kind >= 0 {
                    fallback.get(&self.format).and_then(|f| f.get(&obj.kind))
                } else {
                    None
                }
            })
            .map(Arc::as_ref)
    }

    pub fn decode(
        &self,
        obj: &Object,
        fallback: &Fallback,
        fields: Option<&HashSet<&str>>,
    ) -> Result<Value> {
        let tree = self
            .schema(obj, fallback)
            .with_context(|| format!("No type tree for {} in {}", obj.kind, self.name))?;

        tree.decode(
            &mut Reader::new(&self.data[obj.offset..obj.offset + obj.size], false),
            self.format,
            true,
            fields,
        )
    }
}
