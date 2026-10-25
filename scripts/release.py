from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path
from typing import Any

import httpx
import yaml

INFO_REPOSITORY = "FinnHornhoover/FFInfoPacks"


def github_json(path: str, missing_ok: bool = False) -> dict[str, Any] | None:
    result = subprocess.run(["gh", "api", path], capture_output=True, text=True)
    if result.returncode:
        if missing_ok and "HTTP 404" in result.stderr:
            return None
        raise RuntimeError(result.stderr.strip())
    return json.loads(result.stdout)


def output(name: str, value: str) -> None:
    with open(os.environ["GITHUB_OUTPUT"], "a") as stream:
        stream.write(f"{name}={value}\n")


def main() -> None:
    info = github_json(f"repos/{INFO_REPOSITORY}/releases/latest")
    tag = info["tag_name"]
    names = sorted(
        asset["name"] for asset in info["assets"] if asset["name"].endswith(".zip")
    )
    if not names:
        raise ValueError(f"FFInfoPacks {tag} has no build ZIPs")
    repository = os.environ["GITHUB_REPOSITORY"]
    existing = github_json(f"repos/{repository}/releases/tags/{tag}", missing_ok=True)
    if (
        existing
        and not existing["draft"]
        and sorted(asset["name"] for asset in existing["assets"]) == names
    ):
        print(f"{tag} already matches FFInfoPacks; skipping")
        output("skip_release", "true")
        return

    response = httpx.get(
        f"https://raw.githubusercontent.com/{INFO_REPOSITORY}/main/config/build-config.yml",
        follow_redirects=True,
        timeout=60,
    )
    response.raise_for_status()
    current = yaml.safe_load(response.text)["config"]
    config_root = Path("config/builds")
    config_root.mkdir(parents=True, exist_ok=True)
    for name in names:
        build, version = name.removesuffix(".zip").split("_r", 1)
        revision, _, nickname = version.partition("_")
        entry = {**current[build], "revision": int(revision)}
        entry.pop("nickname", None)
        if nickname:
            entry["nickname"] = nickname
        (config_root / f"{build}.yml").write_text(
            yaml.safe_dump({"config": {build: entry}}, sort_keys=False)
        )
    output("release_tag", tag)
    output("release_name", info["name"] or tag)
    output("info_url", info["html_url"])
    output("skip_release", "false")


if __name__ == "__main__":
    main()
