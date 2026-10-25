from __future__ import annotations

import argparse
import shutil
import zipfile
from pathlib import Path

from tqdm import tqdm

from ffobjects.common import load_config, zip_name


def main(config_path: Path, in_root: Path, out_root: Path) -> None:
    config = load_config(config_path)
    out_root.mkdir(parents=True, exist_ok=True)
    changes = []
    for directory in sorted(in_root.iterdir()):
        if directory.is_dir():
            name = zip_name(directory.name, config[directory.name])
            with (
                zipfile.ZipFile(
                    out_root / name, "w", compression=zipfile.ZIP_DEFLATED
                ) as archive,
                tqdm(
                    sorted(directory.rglob("*")),
                    desc=f"{directory.name}: package",
                    unit="entry",
                    mininterval=2,
                ) as progress,
            ):
                for path in progress:
                    archive.write(path, path.relative_to(directory))
            changes.append(f" - Build `{directory.name}` is available at `{name}`.")
    for build, entry in config.items():
        source = build.removesuffix("-fixed")
        if source != build and source in config:
            name = zip_name(build, entry)
            tqdm.write(f"{build}: copying {source} pack")
            shutil.copyfile(
                out_root / zip_name(source, config[source]), out_root / name
            )
            changes.append(f" - Build `{build}` is available at `{name}`.")
    (out_root / "changelog.txt").write_text("\n".join(changes))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("config", type=Path)
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    main(args.config, args.input, args.output)
