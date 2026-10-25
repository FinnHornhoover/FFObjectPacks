use crate::{progress::Progress, release::Build};
use anyhow::Result;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

pub fn reusable(path: &Path, build: &str, entry: &Build) -> bool {
    (|| -> Result<bool> {
        let mut archive = ZipArchive::new(fs::File::open(path)?)?;
        let mut text = String::new();
        archive
            .by_name("manifest.json")?
            .read_to_string(&mut text)?;

        let metadata: serde_json::Value = serde_json::from_str(&text)?;
        Ok(metadata["format_version"] == crate::FORMAT_VERSION
            && metadata["build"] == build
            && metadata["revision"] == entry.revision)
    })()
    .unwrap_or(false)
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

    let mut progress = Progress::new("package", entries.len());
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
