from __future__ import annotations

import shlex
import subprocess
import sysconfig
from pathlib import Path


def main() -> None:
    directory = Path(__file__).resolve().parent / "ffobjects"
    for name in ("native", "scan"):
        target = directory / f"_{name}{sysconfig.get_config_var('EXT_SUFFIX')}"
        temporary = target.with_suffix(target.suffix + ".new")
        subprocess.run(
            [
                *shlex.split(sysconfig.get_config_var("CXX") or "c++"),
                "-O3",
                "-std=c++17",
                "-shared",
                "-fPIC",
                f"-I{sysconfig.get_path('include')}",
                str(directory / f"{name}.cpp"),
                "-o",
                str(temporary),
            ],
            check=True,
        )
        temporary.replace(target)


if __name__ == "__main__":
    main()
