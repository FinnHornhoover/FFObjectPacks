from __future__ import annotations

import os
import shutil
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from tqdm import tqdm


def script(name: str, *args: Path | str) -> None:
    subprocess.run([sys.executable, f"scripts/{name}.py", *map(str, args)], check=True)


def download(config: Path) -> None:
    script(
        "download_resources",
        config,
        Path("assets") / config.stem,
        Path("artifacts") / config.stem,
    )


def main() -> None:
    configs = sorted(Path("config/builds").glob("*.yml"))
    with ThreadPoolExecutor(max_workers=1) as pool:
        pending = pool.submit(download, configs[0]) if configs else None
        for index, config in enumerate(configs):
            started = time.perf_counter()
            pending.result()
            pending = (
                pool.submit(download, configs[index + 1])
                if index + 1 < len(configs)
                else None
            )
            assets = Path("assets") / config.stem
            output = Path("output") / config.stem
            work = Path("work") / config.stem
            artifacts = Path("artifacts") / config.stem
            script("extract_game_objects", config, assets, output, "--work-root", work)
            script("zip_all_objects", config, output, artifacts)
            subprocess.run(
                [
                    "gh",
                    "release",
                    "upload",
                    os.environ["RELEASE_TAG"],
                    *map(str, sorted(artifacts.glob("*.zip"))),
                    "--repo",
                    os.environ["GITHUB_REPOSITORY"],
                    "--clobber",
                ],
                check=True,
            )
            for directory in (assets, output, work, artifacts):
                shutil.rmtree(directory, ignore_errors=True)
            tqdm.write(
                f"{config.stem}: completed in {time.perf_counter() - started:.0f}s"
            )


if __name__ == "__main__":
    main()
