use crate::{
    convert::Store,
    download,
    pipeline::{Job, Preparation},
    progress::log,
    source::Index,
};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, fs, path::Path, process::Command, sync::Arc};
use tokio::sync::Semaphore;

const INFO_REPOSITORY: &str = "FinnHornhoover/FFInfoPacks";

#[derive(Clone, Deserialize)]
pub struct Build {
    #[serde(default)]
    pub revision: u32,
    #[serde(default)]
    pub nickname: Option<String>,
    pub url: Option<String>,
    #[serde(rename = "api-url")]
    pub api_url: Option<String>,
}

impl Build {
    pub fn zip_name(&self, name: &str) -> String {
        format!(
            "{name}_r{}{}.zip",
            self.revision,
            self.nickname
                .as_ref()
                .map(|n| format!("_{n}"))
                .unwrap_or_default()
        )
    }
}

#[derive(Deserialize)]
struct Config {
    config: BTreeMap<String, Build>,
}

fn gh(args: &[&str]) -> Result<String> {
    let output = Command::new("gh")
        .args(args)
        .output()
        .context("Cannot run GitHub CLI")?;

    ensure!(
        output.status.success(),
        "gh {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );

    Ok(String::from_utf8(output.stdout)?)
}

fn api(path: &str, missing_ok: bool) -> Result<Option<Value>> {
    let output = Command::new("gh").args(["api", path]).output()?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        if missing_ok && error.contains("HTTP 404") {
            return Ok(None);
        }
        bail!("GitHub API {path}: {error}");
    }

    Ok(Some(serde_json::from_slice(&output.stdout)?))
}

fn assets(release: &Value) -> BTreeMap<String, String> {
    release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|asset| {
            Some((
                asset["name"].as_str()?.to_owned(),
                asset["browser_download_url"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned(),
            ))
        })
        .collect()
}

fn matches(release: &Value, names: &BTreeMap<String, String>) -> bool {
    release["draft"] == false
        && release["body"]
            .as_str()
            .unwrap_or("")
            .lines()
            .any(|line| line == format!("Object pack format: {}", crate::FORMAT_VERSION))
        && assets(release)
            .keys()
            .filter(|name| name.ends_with(".zip") && !fixed_zip(name))
            .eq(names.keys())
}

fn fixed_zip(name: &str) -> bool {
    name.strip_suffix(".zip")
        .and_then(|stem| stem.split_once("_r"))
        .is_some_and(|(build, _)| build.ends_with("-fixed"))
}

fn remove_fixed(repository: &str, tag: &str, release: &Value) -> Result<()> {
    for name in assets(release).keys().filter(|name| fixed_zip(name)) {
        log(
            tag,
            "release/cleanup",
            format!("removing fixed archive {name}"),
        );
        gh(&[
            "release",
            "delete-asset",
            tag,
            name,
            "--repo",
            repository,
            "--yes",
        ])?;
    }
    Ok(())
}

pub async fn run() -> Result<()> {
    log(
        "release",
        "discover",
        "reading the latest FFInfoPacks release",
    );

    let repository = std::env::var("GITHUB_REPOSITORY").context("GITHUB_REPOSITORY is required")?;
    let info = api(&format!("repos/{INFO_REPOSITORY}/releases/latest"), false)?.unwrap();
    let tag = info["tag_name"]
        .as_str()
        .context("InfoPacks release has no tag")?;
    let names: BTreeMap<_, _> = assets(&info)
        .into_iter()
        .filter(|(name, _)| name.ends_with(".zip") && !fixed_zip(name))
        .collect();

    ensure!(
        !names.is_empty(),
        "FFInfoPacks {tag} has no nonfixed build ZIPs"
    );

    let existing = api(&format!("repos/{repository}/releases/tags/{tag}"), true)?;
    if let Some(release) = existing.as_ref().filter(|release| matches(release, &names)) {
        remove_fixed(&repository, tag, release)?;

        log(
            tag,
            "release",
            format!(
                "already matches FFInfoPacks and format {}; skipping builds",
                crate::FORMAT_VERSION
            ),
        );
        return Ok(());
    }

    let config = gh(&[
        "api",
        &format!("repos/{INFO_REPOSITORY}/contents/config/build-config.yml"),
        "-H",
        "Accept: application/vnd.github.raw+json",
    ])?;
    let current: Config = serde_yaml::from_str(&config)?;
    let mut builds = BTreeMap::new();
    for name in names.keys() {
        let (build, version) = name
            .trim_end_matches(".zip")
            .split_once("_r")
            .context("Invalid InfoPacks ZIP filename")?;
        let (revision, nickname) = version
            .split_once('_')
            .map(|(r, n)| (r, Some(n.to_owned())))
            .unwrap_or((version, None));
        let mut entry = current
            .config
            .get(build)
            .with_context(|| format!("Build {build} absent from InfoPacks config"))?
            .clone();
        entry.revision = revision.parse()?;
        entry.nickname = nickname;
        builds.insert(build.to_owned(), entry);
    }

    let notes = format!(
        "3D assets for [FFInfoPacks {tag}]({}).\n\nObject pack format: {}\n\nFixed InfoPacks builds use the corresponding nonfixed object pack.",
        info["html_url"].as_str().unwrap_or(""),
        crate::FORMAT_VERSION
    );
    if existing.is_none() {
        gh(&[
            "release",
            "create",
            tag,
            "--repo",
            &repository,
            "--draft",
            "--target",
            &std::env::var("GITHUB_SHA").context("GITHUB_SHA is required")?,
            "--title",
            info["name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(tag),
            "--notes",
            &notes,
        ])?;
    }

    let client = download::client()?;
    let released = api(&format!("repos/{repository}/releases/latest"), true)?
        .filter(|r| {
            r["body"]
                .as_str()
                .unwrap_or("")
                .lines()
                .any(|line| line == format!("Object pack format: {}", crate::FORMAT_VERSION))
        })
        .map(|r| assets(&r))
        .unwrap_or_default();
    let previous = if let Some(url) = released.get("sources.json") {
        let index: Index = serde_json::from_value(download::json(&client, url).await?)?;
        if index.format_version == crate::FORMAT_VERSION {
            index.builds
        } else {
            BTreeMap::new()
        }
    } else {
        BTreeMap::new()
    };
    let mut sources = Index {
        format_version: crate::FORMAT_VERSION,
        builds: BTreeMap::new(),
    };

    log(
        tag,
        "release",
        format!(
            "{} nonfixed builds; preparing at most one ahead",
            builds.len()
        ),
    );

    let store = Arc::new(Store::new(Path::new("conversions"))?);
    let scanner = Arc::new(Semaphore::new(1));
    let workers = rayon::current_num_threads();
    let overlap_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(workers.saturating_sub(1).max(1))
            .build()?,
    );
    let mut jobs = builds.into_iter().map(|(name, entry)| {
        let previous = previous.get(&name).and_then(|record| {
            released
                .get(&record.archive)
                .map(|url| (url.clone(), record.clone()))
        });
        Job {
            name,
            entry,
            previous,
        }
    });
    let mut pending = jobs
        .next()
        .map(|job| Preparation::start(job, client.clone(), scanner.clone()));

    while let Some(mut current) = pending.take() {
        current.wait_download().await?;
        pending = jobs
            .next()
            .map(|job| Preparation::start(job, client.clone(), scanner.clone()));

        let prepared = current.finish().await?;
        let pool = pending.as_ref().map(|_| overlap_pool.clone());
        let completed = prepared.finish(store.clone(), pool).await?;

        log(
            &completed.name,
            "upload",
            format!(
                "started; {}",
                completed.archive.file_name().unwrap().to_string_lossy()
            ),
        );
        gh(&[
            "release",
            "upload",
            tag,
            "--repo",
            &repository,
            "--clobber",
            &completed.archive.to_string_lossy(),
        ])
        .with_context(|| format!("[{}] [upload] failed", completed.name))?;
        log(&completed.name, "upload", "complete");
        sources
            .builds
            .insert(completed.name.clone(), completed.source);
        fs::remove_dir_all(completed.archive.parent().unwrap())?;
        log(
            &completed.name,
            "build",
            format!(
                "complete; {:.1}s including preparation and queue time",
                completed.started.elapsed().as_secs_f64()
            ),
        );
    }

    let index = Path::new("artifacts/sources.json");
    fs::write(index, serde_json::to_vec_pretty(&sources)?)?;
    log(tag, "release/sources", "uploading source fingerprints");
    gh(&[
        "release",
        "upload",
        tag,
        "--repo",
        &repository,
        "--clobber",
        &index.to_string_lossy(),
    ])?;
    fs::remove_file(index)?;

    if let Some(existing) = existing {
        remove_fixed(&repository, tag, &existing)?;
    }

    log(tag, "release", "publishing");
    gh(&[
        "release",
        "edit",
        tag,
        "--repo",
        &repository,
        "--draft=false",
        "--latest",
        "--notes",
        &notes,
    ])?;
    log(tag, "release", "published");
    Ok(())
}
