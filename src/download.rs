use crate::{progress::Progress, release::Build};
use anyhow::{Context, Result, ensure};
use futures_util::{StreamExt, TryStreamExt, stream};
use reqwest::{Client, Url};
use scraper::{Html, Selector};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
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

async fn json(client: &Client, url: &str) -> Result<Value> {
    Ok(request(client, url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
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

async fn resolve(client: &Client, entry: &Build) -> Result<(Value, BTreeMap<String, String>)> {
    let (version, base) = if let Some(api) = &entry.api_url {
        let api = api.trim_end_matches('/');
        let info = json(client, api).await?;
        let id = info["game_versions"][0]
            .as_str()
            .context("Launcher API has no game versions")?;

        let version = json(client, &format!("{api}/versions/{id}.json")).await?;
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
        let text = request(client, &format!("{base}/"))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;

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

    if let Some(main) = version["main_file_url"].as_str() {
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
        files.insert(name, main);
    }

    Ok((version, files))
}

struct Transfer {
    progress: Progress,
    done: usize,
    bytes: u64,
}

async fn transfer(
    client: &Client,
    url: &str,
    path: &Path,
    progress: &Arc<Mutex<Transfer>>,
) -> Result<()> {
    tokio::fs::create_dir_all(path.parent().context("Download has no parent directory")?).await?;
    let temporary = path.with_file_name(format!(
        "{}.part",
        path.file_name().unwrap().to_string_lossy()
    ));

    let result = async {
        let response = request(client, url).send().await?.error_for_status()?;
        let mut chunks = response.bytes_stream();
        let mut file = tokio::fs::File::create(&temporary).await?;

        while let Some(chunk) = chunks.next().await {
            let chunk = chunk?;
            file.write_all(&chunk).await?;

            let mut status = progress.lock().unwrap();
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
        Ok::<_, anyhow::Error>(())
    }
    .await;

    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }

    result.with_context(|| format!("Download {url}"))
}

pub async fn file(client: &Client, url: &str, path: &Path) -> Result<()> {
    let progress = Arc::new(Mutex::new(Transfer {
        progress: Progress::new("download archive", 1),
        done: 0,
        bytes: 0,
    }));

    transfer(client, url, path, &progress).await?;
    progress.lock().unwrap().progress.finish();
    Ok(())
}

pub async fn cache(client: &Client, entry: &Build, root: &Path) -> Result<()> {
    let (version, files) = resolve(client, entry).await?;
    let progress = Arc::new(Mutex::new(Transfer {
        progress: Progress::new("download cache", files.len()),
        done: 0,
        bytes: 0,
    }));

    stream::iter(files.iter())
        .map(|(name, url)| {
            let progress = progress.clone();
            let path = root.join(name);
            async move { transfer(client, url, &path, &progress).await }
        })
        .buffer_unordered(5)
        .try_collect::<Vec<_>>()
        .await?;
    progress.lock().unwrap().progress.finish();

    tokio::fs::write(
        root.join(".cache-manifest.json"),
        serde_json::to_vec_pretty(&version)?,
    )
    .await?;

    tokio::fs::write(
        root.join(".cache-files.json"),
        serde_json::to_vec_pretty(&files.keys().collect::<Vec<_>>())?,
    )
    .await?;
    Ok(())
}
