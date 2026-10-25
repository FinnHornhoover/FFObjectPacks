from __future__ import annotations

import argparse
import shutil
from pathlib import Path

from ffobjects.common import load_config, zip_name


def main(config_path: Path, in_root: Path, out_root: Path) -> None:
    config = load_config(config_path)
    out_root.mkdir(parents=True, exist_ok=True)
    changes = []
    for directory in sorted(in_root.iterdir()):
        if directory.is_dir():
            name = zip_name(directory.name, config[directory.name])
            shutil.make_archive(
                str(out_root / name.removesuffix(".zip")), "zip", directory
            )
            changes.append(f" - Build `{directory.name}` is available at `{name}`.")
    (out_root / "changelog.txt").write_text("\n".join(changes))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("config", type=Path)
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    main(args.config, args.input, args.output)
