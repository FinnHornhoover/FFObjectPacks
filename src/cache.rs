use crate::unity::{
    asset::{Asset, Fallback},
    bundle,
};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct Cache {
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
    pub fallback: Fallback,
}

impl Cache {
    pub fn open(root: &Path) -> Result<Self> {
        let snapshot = root.join(".cache-files.json");
        let mut files = if snapshot.exists() {
            let names: Vec<String> = serde_json::from_slice(&fs::read(snapshot)?)?;
            let mut paths = Vec::new();
            for name in names {
                let path = Path::new(&name);
                ensure!(
                    !path.is_absolute()
                        && path
                            .components()
                            .all(|c| matches!(c, std::path::Component::Normal(_))),
                    "Invalid cache filename {name}"
                );
                let path = root.join(path);
                ensure!(path.is_file(), "Missing cache file {}", path.display());
                paths.push(path);
            }
            paths
        } else {
            let mut paths = Vec::new();
            collect(root, &mut paths)?;
            paths
        };

        files.retain(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("resourceFile" | "unity3d")
            )
        });
        files.sort();

        let mut cache = Self {
            root: root.to_owned(),
            files,
            fallback: Fallback::new(),
        };

        for name in [
            "Tutorial.resourceFile",
            "TrainingGrounds.resourceFile",
            "Map_15_12.unity3d",
        ] {
            let path = root.join(name);
            if !cache.files.contains(&path) {
                continue;
            }
            for asset in cache.assets(&path)?.unwrap_or_default() {
                cache
                    .fallback
                    .entry(asset.format)
                    .or_default()
                    .extend(asset.trees);
            }
        }

        for (id, tree) in cache.fallback.get(&7).cloned().unwrap_or_default() {
            cache
                .fallback
                .entry(6)
                .or_default()
                .entry(id)
                .or_insert(tree);
        }

        Ok(cache)
    }

    pub fn assets(&self, path: &Path) -> Result<Option<Vec<Asset>>> {
        bundle::read(path)?
            .map(|members| {
                members
                    .into_iter()
                    .map(|member| {
                        let name = member.name.clone();
                        Asset::read(member).with_context(|| format!("{} / {name}", path.display()))
                    })
                    .collect()
            })
            .transpose()
    }
}

fn collect(path: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            collect(&entry.path(), output)?;
        } else {
            output.push(entry.path());
        }
    }
    Ok(())
}
