from __future__ import annotations

import argparse
import asyncio
import json
import os
import zipfile
from html.parser import HTMLParser
from pathlib import Path, PurePosixPath
from typing import Any

import aiofiles
import httpx
from tqdm import tqdm
from tqdm.asyncio import tqdm_asyncio

from ffobjects import FORMAT_VERSION
from ffobjects.common import load_config, write_json, zip_name

REPOSITORY = "FinnHornhoover/FFObjectPacks"


class CacheIndex(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.files: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if tag == "a":
            href = dict(attrs).get("href") or ""
            if "/" not in href and href.lower().endswith((".resourcefile", ".unity3d")):
                self.files.append(href)


def headers(url: str) -> dict[str, str]:
    token = os.getenv("GH_TOKEN")
    return (
        {"Authorization": f"Bearer {token}"}
        if token and httpx.URL(url).host == "api.github.com"
        else {}
    )


async def get_json_info(client: httpx.AsyncClient, url: str) -> dict[str, Any]:
    response = await client.get(url, headers=headers(url))
    response.raise_for_status()
    return response.json()


async def download_file(
    client: httpx.AsyncClient, url: str, path: Path, progress: tqdm | None = None
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".part")
    try:
        async with client.stream("GET", url, headers=headers(url)) as response:
            response.raise_for_status()
            if progress is not None:
                progress.total = int(response.headers.get("Content-Length", 0)) or None
            async with aiofiles.open(temporary, "wb") as output:
                async for chunk in response.aiter_bytes(chunk_size=1 << 16):
                    await output.write(chunk)
                    if progress is not None:
                        progress.update(len(chunk))
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def cache_files(version: dict[str, Any], base_url: str) -> dict[str, str]:
    files = {name: f"{base_url.rstrip('/')}/{name}" for name in version["bundles"]}
    if main_url := version.get("main_file_url"):
        source, base = httpx.URL(main_url), httpx.URL(base_url)
        name = PurePosixPath(source.path).name
        if (source.host, source.port) == (
            base.host,
            base.port,
        ) and source.path == base.path.rstrip("/") + "/" + name:
            main_url = f"{base_url.rstrip('/')}/{name}"
        files[name] = main_url
    for name in files:
        path = PurePosixPath(name)
        if (
            not path.parts
            or path.is_absolute()
            or ".." in path.parts
            or "\\" in name
            or str(path) != name
        ):
            raise ValueError(f"Invalid cache filename: {name}")
    return files


async def resolve_cache(
    client: httpx.AsyncClient, entry: dict[str, Any]
) -> tuple[dict[str, Any], dict[str, str]]:
    if "api-url" in entry:
        api = entry["api-url"].rstrip("/")
        info = await get_json_info(client, api)
        version = await get_json_info(
            client, f"{api}/versions/{info['game_versions'][0]}.json"
        )
        return version, cache_files(version, version["asset_url"])
    base_url = entry["url"].rstrip("/")
    response = await client.get(f"{base_url}/")
    response.raise_for_status()
    index = CacheIndex()
    index.feed(response.text)
    if not index.files:
        raise ValueError(f"No cache files listed at {base_url}")
    data = {"asset_url": base_url, "bundles": sorted(set(index.files))}
    return data, cache_files(data, base_url)


def reusable_zip(path: Path, build: str, entry: dict[str, Any]) -> bool:
    try:
        with zipfile.ZipFile(path) as archive:
            metadata = json.loads(archive.read("manifest.json"))
            return (
                metadata.get("format_version") == FORMAT_VERSION
                and metadata.get("build") == build
                and metadata.get("revision") == entry["revision"]
            )
    except (OSError, ValueError, KeyError, zipfile.BadZipFile):
        return False


async def latest_assets(client: httpx.AsyncClient) -> dict[str, str]:
    try:
        release = await get_json_info(
            client, f"https://api.github.com/repos/{REPOSITORY}/releases/latest"
        )
        if (
            f"Object pack format: {FORMAT_VERSION}"
            not in (release.get("body") or "").splitlines()
        ):
            return {}
        return {
            asset["name"]: asset["browser_download_url"] for asset in release["assets"]
        }
    except httpx.HTTPStatusError as error:
        if error.response.status_code == 404:
            return {}
        raise


async def download_build(
    client: httpx.AsyncClient,
    build: str,
    entry: dict[str, Any],
    assets: Path,
    artifacts: Path,
    released: dict[str, str],
) -> None:
    archive = artifacts / zip_name(build, entry)
    if reusable_zip(archive, build, entry):
        tqdm.write(f"{build}: reusing {archive.name}")
        return
    if archive.name in released:
        with tqdm(
            desc=f"{build}: download archive", unit="B", unit_scale=True, mininterval=2
        ) as progress:
            await download_file(client, released[archive.name], archive, progress)
        if reusable_zip(archive, build, entry):
            return
        archive.unlink()
    version, files = await resolve_cache(client, entry)
    directory = assets / build
    await tqdm_asyncio.gather(
        *[download_file(client, url, directory / name) for name, url in files.items()],
        desc=f"{build}: download cache",
        unit="file",
        mininterval=2,
    )
    write_json(directory / ".cache-manifest.json", version)
    write_json(directory / ".cache-files.json", sorted(files))


async def main(config_path: Path, asset_root: Path, artifact_root: Path) -> None:
    config = load_config(config_path)
    asset_root.mkdir(parents=True, exist_ok=True)
    artifact_root.mkdir(parents=True, exist_ok=True)
    async with httpx.AsyncClient(
        limits=httpx.Limits(max_connections=5),
        timeout=httpx.Timeout(None),
        follow_redirects=True,
    ) as client:
        released = await latest_assets(client)
        targets = [
            download_build(
                client,
                build,
                entry,
                asset_root,
                artifact_root,
                released,
            )
            for build, entry in config.items()
            if not (build.endswith("-fixed") and build.removesuffix("-fixed") in config)
        ]
        await asyncio.gather(*targets)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("config", type=Path)
    parser.add_argument("assets", type=Path)
    parser.add_argument("artifacts", type=Path)
    args = parser.parse_args()
    asyncio.run(main(args.config, args.assets, args.artifacts))
