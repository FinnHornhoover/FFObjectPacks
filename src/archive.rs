use crate::{progress::Progress, release::Build, source::Record};
use anyhow::Result;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

pub fn reusable(path: &Path, build: &str, fingerprint: &str) -> bool {
    (|| -> Result<bool> {
        let mut archive = ZipArchive::new(fs::File::open(path)?)?;
        let mut text = String::new();
        archive
            .by_name("manifest.json")?
            .read_to_string(&mut text)?;

        let metadata: serde_json::Value = serde_json::from_str(&text)?;
        Ok(metadata["format_version"] == crate::FORMAT_VERSION
            && metadata["build"] == build
            && metadata["source_fingerprint"] == fingerprint)
    })()
    .unwrap_or(false)
}

pub fn stamp(root: &Path, fingerprint: &str) -> Result<()> {
    let path = root.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&path)?)?;
    manifest["source_fingerprint"] = fingerprint.into();
    fs::write(path, serde_json::to_vec(&manifest)?)?;
    Ok(())
}

pub fn rebase(
    previous: &Path,
    target: &Path,
    entry: &Build,
    source: &Record,
    version: &Value,
) -> Result<()> {
    let mut previous = ZipArchive::new(fs::File::open(previous)?)?;
    let mut manifest: Value = serde_json::from_reader(previous.by_name("manifest.json")?)?;
    manifest["revision"] = entry.revision.into();
    manifest["nickname"] = json!(entry.nickname);
    manifest["source_fingerprint"] = source.fingerprint.clone().into();
    manifest["cache_uuid"] = version["uuid"].clone();

    let mut replacements = BTreeMap::from([
        ("manifest.json", serde_json::to_vec(&manifest)?),
        (
            "metadata/cache-manifest.json",
            serde_json::to_vec_pretty(version)?,
        ),
        (
            "metadata/cache-files.json",
            serde_json::to_vec_pretty(&source.files.keys().collect::<Vec<_>>())?,
        ),
    ]);
    let mut archive = ZipWriter::new(fs::File::create(target)?);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(6))
        .unix_permissions(0o644);
    let mut progress = Progress::for_path(
        target.parent().unwrap(),
        "finish/reuse",
        previous.len(),
        "files",
    );
    for i in 0..previous.len() {
        let file = previous.by_index(i)?;
        if let Some(data) = replacements.remove(file.name()) {
            archive.start_file(file.name(), options)?;
            archive.write_all(&data)?;
        } else {
            archive.raw_copy_file(file)?;
        }
        progress.update(i + 1, "");
    }
    for (name, data) in replacements {
        archive.start_file(name, options)?;
        archive.write_all(&data)?;
    }
    archive.finish()?;
    progress.finish();
    Ok(())
}

fn files(root: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            files(&entry.path(), output)?;
        } else {
            output.push(entry.path());
        }
    }
    Ok(())
}

pub fn create(root: &Path, path: &Path) -> Result<()> {
    let mut entries = Vec::new();
    files(root, &mut entries)?;
    entries.sort();

    fs::create_dir_all(path.parent().unwrap())?;
    let mut archive = ZipWriter::new(fs::File::create(path)?);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(6))
        .unix_permissions(0o644);

    let mut progress = Progress::for_path(root, "finish/package", entries.len(), "files");
    let mut buffer = vec![0; 128 * 1024];
    for (index, entry) in entries.iter().enumerate() {
        archive.start_file(
            entry
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/"),
            options,
        )?;
        let mut file = fs::File::open(entry)?;
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            archive.write_all(&buffer[..n])?;
        }
        progress.update(index + 1, "");
    }

    archive.finish()?;
    progress.finish();
    Ok(())
}
