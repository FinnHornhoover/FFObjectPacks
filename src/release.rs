use crate::{archive, convert::Store, download, pack};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, fs, path::Path, process::Command, time::Instant};

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
        && assets(release).keys().eq(names.keys())
}

fn remove(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

pub async fn run() -> Result<()> {
    let repository = std::env::var("GITHUB_REPOSITORY").context("GITHUB_REPOSITORY is required")?;
    let info = api(&format!("repos/{INFO_REPOSITORY}/releases/latest"), false)?.unwrap();
    let tag = info["tag_name"]
        .as_str()
        .context("InfoPacks release has no tag")?;
    let names: BTreeMap<_, _> = assets(&info)
        .into_iter()
        .filter(|(name, _)| name.ends_with(".zip"))
        .collect();

    ensure!(!names.is_empty(), "FFInfoPacks {tag} has no build ZIPs");

    let existing = api(&format!("repos/{repository}/releases/tags/{tag}"), true)?;
    if existing
        .as_ref()
        .is_some_and(|release| matches(release, &names))
    {
        eprintln!(
            "{tag} already matches FFInfoPacks and format {}; skipping",
            crate::FORMAT_VERSION
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

    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for build in builds.keys() {
        let source = build
            .strip_suffix("-fixed")
            .filter(|source| builds.contains_key(*source))
            .unwrap_or(build);
        groups
            .entry(source.to_owned())
            .or_default()
            .push(build.clone());
    }

    let notes = format!(
        "3D assets for [FFInfoPacks {tag}]({}).\n\nObject pack format: {}",
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
    let store = Store::new(Path::new("conversions"))?;

    for (source, variants) in groups {
        let started = Instant::now();
        eprintln!("{source}: starting");

        let entry = &builds[&source];
        let cache = Path::new("assets").join(&source);
        let output = Path::new("output").join(&source);
        let artifacts = Path::new("artifacts").join(&source);
        fs::create_dir_all(&artifacts)?;
        let archive = artifacts.join(entry.zip_name(&source));

        if !archive::reusable(&archive, &source, entry) {
            if let Some(url) = released.get(&entry.zip_name(&source)) {
                download::file(&client, url, &archive).await?;
            }

            if !archive::reusable(&archive, &source, entry) {
                download::cache(&client, entry, &cache).await?;
                pack::extract(
                    &source,
                    entry.revision,
                    entry.nickname.as_deref(),
                    &cache,
                    &output,
                    &store,
                    None,
                )?;

                remove(&cache)?;
                archive::create(&output, &archive)?;
                remove(&output)?;
            }
        }

        let mut archives = vec![archive.clone()];
        for variant in variants.iter().filter(|v| *v != &source) {
            let copy = artifacts.join(builds[variant].zip_name(variant));
            fs::copy(&archive, &copy)?;
            archives.push(copy);
            eprintln!("{variant}: copied {source} pack");
        }

        let mut args = vec![
            "release".to_owned(),
            "upload".into(),
            tag.into(),
            "--repo".into(),
            repository.clone(),
            "--clobber".into(),
        ];
        args.extend(archives.iter().map(|p| p.to_string_lossy().into_owned()));
        gh(&args.iter().map(String::as_str).collect::<Vec<_>>())?;

        remove(&artifacts)?;
        eprintln!(
            "{source}: completed in {:.1}s",
            started.elapsed().as_secs_f64()
        );
    }

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
    Ok(())
}
