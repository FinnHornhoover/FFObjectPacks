use crate::{
    archive, download,
    progress::{disk_usage, log},
    release::Build,
    source::Record,
};
use anyhow::{Context, Result, ensure};
use reqwest::Client;
use serde_json::Value;
use std::{fs, path::PathBuf, process::Stdio, time::Instant};
use tokio::{process::Command, task::JoinHandle};

pub struct Job {
    pub name: String,
    pub entry: Build,
    pub previous: Option<(String, Record)>,
}

impl Job {
    fn cache(&self) -> PathBuf {
        PathBuf::from("assets").join(&self.name)
    }

    fn output(&self) -> PathBuf {
        PathBuf::from("output").join(&self.name)
    }

    fn archive(&self) -> PathBuf {
        PathBuf::from("artifacts")
            .join(&self.name)
            .join(self.entry.zip_name(&self.name))
    }
}

pub struct Preparation {
    task: JoinHandle<Result<Prepared>>,
}

impl Preparation {
    pub fn start(job: Job, client: Client) -> Self {
        let name = job.name.clone();
        let task = tokio::spawn(async move {
            prepare(job, client)
                .await
                .with_context(|| format!("[{name}] [prepare] failed"))
        });
        Self { task }
    }

    pub async fn finish(mut self) -> Result<Prepared> {
        (&mut self.task).await?
    }
}

impl Drop for Preparation {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct Prepared {
    job: Job,
    extract: bool,
    source: Record,
    manifest: Value,
    started: Instant,
}

async fn prepare(job: Job, client: Client) -> Result<Prepared> {
    let started = Instant::now();
    log(&job.name, "prepare", "started");

    let archive = job.archive();
    fs::create_dir_all(archive.parent().unwrap())?;
    let resolved = download::resolve(&client, &job.entry, &job.name).await?;
    let cache = job.cache();
    let mut cache_downloaded = false;
    let hashes = if let Some(hashes) = &resolved.hashes {
        log(&job.name, "prepare/source", "using published file hashes");
        hashes.clone()
    } else {
        log(
            &job.name,
            "prepare/source",
            "hashing source files during download",
        );
        cache_downloaded = true;
        download::cache(&client, &resolved, &cache).await?
    };
    let mut source = Record::new(job.entry.zip_name(&job.name), hashes)?;
    let previous = archive.parent().unwrap().join("previous.zip");
    let mut reuse = false;
    if let Some((url, record)) = &job.previous {
        if record.fingerprint == source.fingerprint {
            log(
                &job.name,
                "prepare/source",
                "source files unchanged; reusing previous payloads",
            );
            download::file(&client, url, &previous).await?;
            reuse = archive::reusable(&previous, &job.name, &source.fingerprint);
            if !reuse {
                log(
                    &job.name,
                    "prepare/source",
                    "previous archive metadata does not match; extracting source",
                );
                fs::remove_file(&previous)?;
            }
        } else {
            let added = source
                .files
                .keys()
                .filter(|name| !record.files.contains_key(*name))
                .count();
            let removed = record
                .files
                .keys()
                .filter(|name| !source.files.contains_key(*name))
                .count();
            let changed = source
                .files
                .iter()
                .filter(|(name, hash)| record.files.get(*name).is_some_and(|old| old != *hash))
                .count();
            log(
                &job.name,
                "prepare/source",
                format!(
                    "source changed; {added} added, {removed} removed, {changed} modified files"
                ),
            );
        }
    } else {
        log(
            &job.name,
            "prepare/source",
            "no source baseline for this format; extracting source",
        );
    }

    if !reuse && !cache_downloaded {
        source = Record::new(
            job.entry.zip_name(&job.name),
            download::cache(&client, &resolved, &cache).await?,
        )?;
    }

    log(&job.name, "prepare", "ready; awaiting finish stage");

    Ok(Prepared {
        job,
        extract: !reuse,
        source,
        manifest: resolved.manifest,
        started,
    })
}

pub struct Completed {
    pub name: String,
    pub archive: PathBuf,
    pub source: Record,
    pub started: Instant,
}

impl Prepared {
    pub async fn finish(self) -> Result<Completed> {
        let Self {
            job,
            extract,
            source,
            manifest,
            started,
        } = self;
        let name = job.name.clone();
        disk_usage(&name, "finish start");

        if extract {
            let mut command = Command::new(std::env::current_exe()?);
            command
                .arg("extract")
                .arg(job.cache())
                .arg(job.output())
                .arg("--build")
                .arg(&name)
                .arg("--revision")
                .arg(job.entry.revision.to_string())
                .stdin(Stdio::null())
                .kill_on_drop(true);
            if let Some(nickname) = &job.entry.nickname {
                command.arg("--nickname").arg(nickname);
            }

            let mut worker = command
                .spawn()
                .with_context(|| format!("[{name}] [worker] cannot start extractor"))?;
            log(
                &name,
                "worker",
                format!(
                    "started; pid={}; scan and extraction in a fresh process",
                    worker.id().unwrap()
                ),
            );
            let status = worker
                .wait()
                .await
                .with_context(|| format!("[{name}] [worker] cannot wait for extractor"))?;
            ensure!(
                status.success(),
                "[{name}] [worker] extraction failed: {status}"
            );
            log(&name, "worker", "complete; process memory released");
        }

        tokio::task::spawn_blocking(move || -> Result<Completed> {
            let archive = job.archive();
            if extract {
                let output = job.output();
                archive::stamp(&output, &source.fingerprint)?;
                fs::remove_dir_all(job.cache())?;
                disk_usage(&job.name, "before packaging; source cache removed");
                archive::create(&output, &archive)?;
                fs::remove_dir_all(output)?;
            } else {
                log(
                    &job.name,
                    "finish",
                    "updating revision metadata; copying compressed payloads",
                );
                archive::rebase(
                    &archive.parent().unwrap().join("previous.zip"),
                    &archive,
                    &job.entry,
                    &source,
                    &manifest,
                )?;
                if job.cache().exists() {
                    fs::remove_dir_all(job.cache())?;
                }
            }

            log(&job.name, "finish", "archive ready for upload");
            Ok(Completed {
                name: job.name,
                archive,
                source,
                started,
            })
        })
        .await?
        .with_context(|| format!("[{name}] [finish] failed"))
    }
}
