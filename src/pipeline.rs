use crate::{
    archive, convert::Store, download, pack, progress::log, release::Build, source::Record,
};
use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::Value;
use std::{fs, path::PathBuf, sync::Arc, time::Instant};
use tokio::{
    sync::{Semaphore, oneshot},
    task::JoinHandle,
};

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
    downloaded: oneshot::Receiver<()>,
    task: JoinHandle<Result<Prepared>>,
}

impl Preparation {
    pub fn start(job: Job, client: Client, scanner: Arc<Semaphore>) -> Self {
        let (downloaded, ready) = oneshot::channel();
        let name = job.name.clone();
        let task = tokio::spawn(async move {
            prepare(job, client, scanner, downloaded)
                .await
                .with_context(|| format!("[{name}] [prepare] failed"))
        });
        Self {
            downloaded: ready,
            task,
        }
    }

    pub async fn wait_download(&mut self) -> Result<()> {
        if (&mut self.downloaded).await.is_err() {
            (&mut self.task).await??;
            bail!("Preparation ended before reporting download completion");
        }
        Ok(())
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
    extraction: Option<pack::Prepared>,
    source: Record,
    manifest: Value,
    started: Instant,
}

async fn prepare(
    job: Job,
    client: Client,
    scanner: Arc<Semaphore>,
    downloaded: oneshot::Sender<()>,
) -> Result<Prepared> {
    let started = Instant::now();
    log(&job.name, "prepare", "started");

    let archive = job.archive();
    fs::create_dir_all(archive.parent().unwrap())?;
    let resolved = download::resolve(&client, &job.entry).await?;
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

    let extraction = if reuse {
        let _ = downloaded.send(());
        None
    } else {
        if !cache_downloaded {
            source = Record::new(
                job.entry.zip_name(&job.name),
                download::cache(&client, &resolved, &cache).await?,
            )?;
        }
        let output = job.output();

        // Acquire before notifying the driver so the next download overlaps this scan.
        if scanner.available_permits() == 0 {
            log(
                &job.name,
                "prepare/wait",
                "cache downloaded; waiting for the current scan to finish",
            );
        }
        let permit = scanner.acquire_owned().await?;
        let _ = downloaded.send(());
        let prepared = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            pack::prepare(&cache, &output, None)
        })
        .await??;

        Some(prepared)
    };

    log(&job.name, "prepare", "ready; awaiting finish stage");

    Ok(Prepared {
        job,
        extraction,
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
    pub async fn finish(
        self,
        store: Arc<Store>,
        pool: Option<Arc<rayon::ThreadPool>>,
    ) -> Result<Completed> {
        let name = self.job.name.clone();
        tokio::task::spawn_blocking(move || -> Result<Completed> {
            let Self {
                job,
                extraction,
                source,
                manifest,
                started,
            } = self;
            let archive = job.archive();

            if let Some(prepared) = extraction {
                let workers = pool
                    .as_ref()
                    .map(|pool| pool.current_num_threads())
                    .unwrap_or_else(rayon::current_num_threads);
                log(
                    &job.name,
                    "finish",
                    format!("started; {workers} conversion threads"),
                );

                let output = job.output();
                let extract = || {
                    pack::finish(
                        &job.name,
                        job.entry.revision,
                        job.entry.nickname.as_deref(),
                        prepared,
                        &output,
                        &store,
                    )
                };

                if let Some(pool) = pool {
                    pool.install(extract)?;
                } else {
                    extract()?;
                }

                archive::stamp(&output, &source.fingerprint)?;
                fs::remove_dir_all(job.cache())?;
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
