mod animation;
mod archive;
mod cache;
mod convert;
mod download;
mod geometry;
mod pack;
mod plan;
mod progress;
mod release;
mod scan;
mod terrain;
mod texture;
mod unity;
mod value;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

pub const FORMAT_VERSION: u32 = 2;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Publish packs matching the latest FFInfoPacks release")]
    Release,
    #[command(about = "Write scan metadata for inspection")]
    Scan {
        cache: PathBuf,
        output: PathBuf,
        #[arg(long)]
        work: PathBuf,
    },
    #[command(about = "Extract a local offline cache")]
    Extract {
        cache: PathBuf,
        output: PathBuf,
        #[arg(long)]
        build: String,
        #[arg(long)]
        revision: u32,
        #[arg(long)]
        nickname: Option<String>,
        #[arg(long, default_value = "conversions")]
        conversions: PathBuf,
        #[arg(long)]
        scan_work: Option<PathBuf>,
    },
}

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<()> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(
            std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1)
                .min(4),
        )
        .build_global()?;

    match Cli::parse().command {
        Command::Release => release::run().await?,
        Command::Scan {
            cache,
            output,
            work,
        } => {
            let cache = cache::Cache::open(&cache)?;
            scan::Scan::run(&cache, &output, Some(&work))?;
        }
        Command::Extract {
            cache,
            output,
            build,
            revision,
            nickname,
            conversions,
            scan_work,
        } => {
            let store = convert::Store::new(&conversions)?;
            pack::extract(
                &build,
                revision,
                nickname.as_deref(),
                &cache,
                &output,
                &store,
                scan_work.as_deref(),
            )?;
        }
    }
    Ok(())
}
