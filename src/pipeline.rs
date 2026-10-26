use crate::{archive, convert::Store, download, pack, progress::log, release::Build};
use anyhow::{Context, Result, bail};
use reqwest::Client;
use std::{fs, path::PathBuf, sync::Arc, time::Instant};
use tokio::{
    sync::{Semaphore, oneshot},
    task::JoinHandle,
};

pub struct Job {
    pub name: String,
    pub entry: Build,
    pub released: Option<String>,
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
    if !archive::reusable(&archive, &job.name, &job.entry)
        && let Some(url) = &job.released
    {
        download::file(&client, url, &archive).await?;
    }

    let extraction = if archive::reusable(&archive, &job.name, &job.entry) {
        let _ = downloaded.send(());
        log(
            &job.name,
            "prepare/archive",
            "existing archive accepted; extraction skipped",
        );
        None
    } else {
        let cache = job.cache();
        let output = job.output();
        download::cache(&client, &job.entry, &cache).await?;

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
        started,
    })
}

pub struct Completed {
    pub name: String,
    pub archive: PathBuf,
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

                fs::remove_dir_all(job.cache())?;
                archive::create(&output, &archive)?;
                fs::remove_dir_all(output)?;
            } else {
                log(
                    &job.name,
                    "finish",
                    "using existing archive; extraction and packaging skipped",
                );
            }

            log(&job.name, "finish", "archive ready for upload");
            Ok(Completed {
                name: job.name,
                archive,
                started,
            })
        })
        .await?
        .with_context(|| format!("[{name}] [finish] failed"))
    }
}
