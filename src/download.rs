use crate::{
    progress::{Progress, log},
    release::Build,
};
use anyhow::{Context, Result, ensure};
use futures_util::{StreamExt, TryStreamExt, stream};
use reqwest::{Client, Url};
use scraper::{Html, Selector};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    future::Future,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::AsyncWriteExt;

pub fn client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent("FFObjectPacks")
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(120))
        .build()?)
}

fn request(client: &Client, url: &str) -> reqwest::RequestBuilder {
    let mut request = client.get(url);
    if Url::parse(url).is_ok_and(|u| u.host_str() == Some("api.github.com"))
        && let Ok(token) = std::env::var("GH_TOKEN")
    {
        request = request.bearer_auth(token);
    }

    request
}

const ATTEMPTS: usize = 5;

fn retry_delay(attempt: usize, error: &anyhow::Error) -> Option<Duration> {
    if attempt >= ATTEMPTS {
        return None;
    }
    let error = error.downcast_ref::<reqwest::Error>()?;
    let transient = if let Some(status) = error.status() {
        status.is_server_error() || matches!(status.as_u16(), 408 | 429)
    } else {
        error.is_connect()
            || error.is_timeout()
            || error.is_body()
            || error.is_decode()
            || error.is_request()
    };
    transient.then(|| Duration::from_secs(1 << (attempt - 1)))
}

async fn retry<T, F, Fut>(
    mut action: F,
    mut report: impl FnMut(usize, Duration, &anyhow::Error),
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut attempt = 1;
    loop {
        match action().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                let Some(delay) = retry_delay(attempt, &error) else {
                    return Err(error);
                };
                report(attempt, delay, &error);
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

async fn read(client: &Client, url: &str, build: &str, operation: &str) -> Result<Vec<u8>> {
    retry(
        || async {
            Ok(request(client, url)
                .send()
                .await?
                .error_for_status()?
                .bytes()
                .await?
                .to_vec())
        },
        |attempt, delay, error| {
            log(
                build,
                operation,
                format!(
                    "{url}: attempt {attempt}/{ATTEMPTS} failed; retrying in {}s: {error:#}",
                    delay.as_secs()
                ),
            )
        },
    )
    .await
    .with_context(|| format!("Request {url}"))
}

pub async fn json(client: &Client, url: &str) -> Result<Value> {
    Ok(serde_json::from_slice(
        &read(client, url, "release", "release/sources").await?,
    )?)
}

fn safe_name(name: &str) -> Result<()> {
    let path = Path::new(name);
    ensure!(
        !name.is_empty()
            && !name.contains('\\')
            && !path.is_absolute()
            && path
                .components()
                .all(|p| matches!(p, std::path::Component::Normal(_))),
        "Invalid cache filename {name}"
    );
    Ok(())
}

pub struct Source {
    pub manifest: Value,
    pub files: BTreeMap<String, String>,
    pub hashes: Option<BTreeMap<String, String>>,
}

pub async fn resolve(client: &Client, entry: &Build, build: &str) -> Result<Source> {
    let (version, base) = if let Some(api) = &entry.api_url {
        let api = api.trim_end_matches('/');
        let info: Value =
            serde_json::from_slice(&read(client, api, build, "prepare/source").await?)?;
        let id = info["game_versions"][0]
            .as_str()
            .context("Launcher API has no game versions")?;

        let version: Value = serde_json::from_slice(
            &read(
                client,
                &format!("{api}/versions/{id}.json"),
                build,
                "prepare/source",
            )
            .await?,
        )?;
        let base = version["asset_url"]
            .as_str()
            .context("Manifest has no asset_url")?
            .to_owned();
        (version, base)
    } else {
        let base = entry
            .url
            .as_deref()
            .context("Build has neither url nor api-url")?
            .trim_end_matches('/')
            .to_owned();
        let text =
            String::from_utf8(read(client, &format!("{base}/"), build, "prepare/source").await?)?;

        let names = {
            let html = Html::parse_document(&text);
            let selector = Selector::parse("a[href]").unwrap();
            let mut names: Vec<_> = html
                .select(&selector)
                .filter_map(|a| a.value().attr("href"))
                .filter(|s| {
                    !s.contains('/')
                        && (s.to_ascii_lowercase().ends_with(".resourcefile")
                            || s.to_ascii_lowercase().ends_with(".unity3d"))
                })
                .map(str::to_owned)
                .collect();
            names.sort();
            names.dedup();
            names
        };
        ensure!(!names.is_empty(), "No cache files listed at {base}");
        (json!({ "asset_url": base, "bundles": names }), base)
    };

    let base = base.trim_end_matches('/');
    let mut files = BTreeMap::new();
    let bundles = &version["bundles"];
    let names: Vec<&str> = match bundles {
        Value::Object(map) => map.keys().map(String::as_str).collect(),
        Value::Array(list) => list.iter().filter_map(Value::as_str).collect(),
        _ => anyhow::bail!("Manifest has no bundle list"),
    };
    for name in names {
        safe_name(name)?;
        files.insert(name.to_owned(), format!("{base}/{name}"));
    }

    let main_name = if let Some(main) = version["main_file_url"].as_str() {
        let url = Url::parse(main)?;
        let name = url
            .path_segments()
            .and_then(Iterator::last)
            .context("Invalid main file URL")?
            .to_owned();
        safe_name(&name)?;

        let base_url = Url::parse(base)?;
        let main = if url.host_str() == base_url.host_str()
            && url.port() == base_url.port()
            && url.path() == format!("{}/{name}", base_url.path().trim_end_matches('/'))
        {
            format!("{base}/{name}")
        } else {
            main.to_owned()
        };
        files.insert(name.clone(), main);
        Some(name)
    } else {
        None
    };

    let hashes = files
        .keys()
        .map(|name| {
            let value = if main_name.as_ref() == Some(name) {
                &version["main_file_info"]["hash"]
            } else {
                &version["bundles"][name]["compressed_info"]["hash"]
            };
            let hash = value.as_str()?;
            (hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()))
                .then(|| (name.clone(), hash.to_ascii_lowercase()))
        })
        .collect();

    Ok(Source {
        manifest: version,
        files,
        hashes,
    })
}

struct Transfer {
    progress: Progress,
    done: usize,
    bytes: u64,
}

async fn transfer_once(
    client: &Client,
    url: &str,
    path: &Path,
    progress: &Arc<Mutex<Transfer>>,
) -> Result<String> {
    tokio::fs::create_dir_all(path.parent().context("Download has no parent directory")?).await?;
    let temporary = path.with_file_name(format!(
        "{}.part",
        path.file_name().unwrap().to_string_lossy()
    ));

    let mut received = 0;
    let result = async {
        let response = request(client, url).send().await?.error_for_status()?;
        let mut chunks = response.bytes_stream();
        let mut file = tokio::fs::File::create(&temporary).await?;
        let mut digest = Sha256::new();

        while let Some(chunk) = chunks.next().await {
            let chunk = chunk?;
            file.write_all(&chunk).await?;
            digest.update(&chunk);

            let mut status = progress.lock().unwrap();
            received += chunk.len() as u64;
            status.bytes += chunk.len() as u64;
            let done = status.done;
            let bytes = status.bytes;
            status
                .progress
                .update(done, format!("{:.1} MiB", bytes as f64 / 1048576.0));
        }

        file.flush().await?;
        drop(file);

        tokio::fs::rename(&temporary, path).await?;
        progress.lock().unwrap().done += 1;
        Ok::<_, anyhow::Error>(hex::encode(digest.finalize()))
    }
    .await;

    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
        progress.lock().unwrap().bytes -= received;
    }

    result.with_context(|| format!("Download {url}"))
}

async fn transfer(
    client: &Client,
    url: &str,
    path: &Path,
    progress: &Arc<Mutex<Transfer>>,
) -> Result<String> {
    retry(
        || transfer_once(client, url, path, progress),
        |attempt, delay, error| {
            progress.lock().unwrap().progress.message(format!(
                "{}: attempt {attempt}/{ATTEMPTS} failed; retrying in {}s: {error:#}",
                path.file_name().unwrap().to_string_lossy(),
                delay.as_secs()
            ))
        },
    )
    .await
}

pub async fn file(client: &Client, url: &str, path: &Path) -> Result<()> {
    let progress = Arc::new(Mutex::new(Transfer {
        progress: Progress::for_path(path.parent().unwrap(), "prepare/archive", 1, "files"),
        done: 0,
        bytes: 0,
    }));

    transfer(client, url, path, &progress).await?;
    progress.lock().unwrap().progress.finish();
    Ok(())
}

pub async fn cache(
    client: &Client,
    source: &Source,
    root: &Path,
) -> Result<BTreeMap<String, String>> {
    let files = &source.files;
    let progress = Arc::new(Mutex::new(Transfer {
        progress: Progress::for_path(root, "prepare/download", files.len(), "files"),
        done: 0,
        bytes: 0,
    }));

    let targets: Vec<_> = files
        .iter()
        .map(|(name, url)| (name.clone(), root.join(name), url.clone()))
        .collect();
    let hashes = stream::iter(targets)
        .map(|(name, path, url)| {
            let progress = progress.clone();
            let client = client.clone();
            async move {
                let hash = transfer(&client, &url, &path, &progress).await?;
                Ok::<_, anyhow::Error>((name, hash))
            }
        })
        .buffer_unordered(5)
        .try_collect::<BTreeMap<_, _>>()
        .await?;
    progress.lock().unwrap().progress.finish();

    tokio::fs::write(
        root.join(".cache-manifest.json"),
        serde_json::to_vec_pretty(&source.manifest)?,
    )
    .await?;

    tokio::fs::write(
        root.join(".cache-files.json"),
        serde_json::to_vec_pretty(&files.keys().collect::<Vec<_>>())?,
    )
    .await?;
    Ok(hashes)
}
