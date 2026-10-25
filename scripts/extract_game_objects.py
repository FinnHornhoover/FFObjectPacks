from __future__ import annotations

import argparse
from pathlib import Path

from ffobjects.common import load_config
from ffobjects.extractor import extract_build, finish_build, scan_build


def main(
    config_path: Path,
    asset_root: Path,
    out_root: Path,
    work_root: Path,
    conversion_cache: Path,
    phase: str = "all",
    workers: int | None = None,
) -> None:
    config = load_config(config_path)
    out_root.mkdir(parents=True, exist_ok=True)
    for cache in sorted(asset_root.iterdir()):
        if cache.is_dir() and cache.name in config:
            if phase == "scan":
                scan_build(cache, out_root / cache.name, work_root / cache.name)
                continue
            operation = finish_build if phase == "finish" else extract_build
            operation(
                cache.name,
                config[cache.name],
                cache,
                out_root / cache.name,
                work_root / cache.name,
                conversion_cache,
                workers,
            )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(
        description="Extract GLB geometry, lossless WebP, source animations and terrain"
    )
    parser.add_argument("config", type=Path)
    parser.add_argument("assets", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--work-root", type=Path, default=Path("work"))
    parser.add_argument("--conversion-cache", type=Path, default=Path("conversions"))
    parser.add_argument("--phase", choices=("all", "scan", "finish"), default="all")
    parser.add_argument("--workers", type=int)
    args = parser.parse_args()
    main(
        args.config,
        args.assets,
        args.output,
        args.work_root,
        args.conversion_cache,
        args.phase,
        args.workers,
    )
