from __future__ import annotations

import collections
import multiprocessing
import os
import json
import re
import shutil
import struct
from concurrent.futures import FIRST_COMPLETED, ProcessPoolExecutor, wait
from pathlib import Path, PurePosixPath
from typing import Any

from tqdm import tqdm

from . import FORMAT_VERSION
from .cache import CacheReader
from .common import pack, write_json
from .conversion import convert_bundle, initialize_worker
from .planner import plan
from .scanner import Scanner

COORDINATES = {
    "hierarchies": "Unity Y-up left-handed source TRS",
    "geometry": "glTF right-handed, reflected X; reverse winding; tangent X and W negated",
    "conversion": "Reflect hierarchy positions X and quaternion Y/Z when building a glTF scene; conjugate bind matrices by diag(-1,1,1,1)",
    "uv": "Source Unity UVs; texture images vertically flipped from Unity storage; use flipY=false with glTF",
    "terrain": "Original sample order, dimensions, scale and signedness; reconstruct in source coordinates before conversion",
}


def export_assets(
    selection: list[dict[str, Any]],
    reader: CacheReader,
    output: Path,
    conversion_cache: Path,
) -> dict[int, dict[str, Any]]:
    by_file = collections.defaultdict(list)
    for record in selection:
        by_file[record["source"]].append(record)
    results, errors, reused = {}, [], 0
    workers = min(4, os.process_cpu_count() or 1)
    tasks = iter(sorted(by_file.items(), key=lambda item: -len(item[1])))
    with (
        tqdm(
            total=len(selection),
            desc=f"{reader.root.name}: extract",
            unit="asset",
            mininterval=2,
        ) as progress,
        ProcessPoolExecutor(
            max_workers=workers,
            mp_context=multiprocessing.get_context("spawn"),
            initializer=initialize_worker,
            initargs=(
                reader.root,
                reader.spool,
                reader.fallback,
                output,
                conversion_cache,
            ),
        ) as pool,
    ):
        pending = {}

        def submit() -> None:
            item = next(tasks, None)
            if item is not None:
                filename, records = item
                pending[pool.submit(convert_bundle, filename, records)] = len(records)

        for _ in range(workers):
            submit()
        while pending:
            done, _ = wait(pending, timeout=2, return_when=FIRST_COMPLETED)
            for future in done:
                count = pending.pop(future)
                converted, failures, hits = future.result()
                results.update(converted)
                errors.extend(failures)
                reused += hits
                progress.update(count)
                submit()
            progress.set_postfix(reused=reused, errors=len(errors), refresh=False)
            progress.refresh()
    expected = {record["object"] for record in selection}
    if errors or set(results) != expected:
        write_json(
            output / "metadata/extraction-errors.json",
            {"errors": errors, "missing": sorted(expected - results.keys())},
        )
        raise ValueError("Visual asset extraction failed; no archive will be produced")
    return results


def write_catalogs(planned: dict[str, Any], output: Path) -> None:
    node_data = (output / "metadata/node-map.bin").read_bytes()
    node_map = {
        oid: (graph, node)
        for oid, graph, node in struct.iter_unpack("<IHI", node_data[8:])
    }
    by_category = collections.defaultdict(list)
    seen = set()
    for entry in tqdm(
        planned["catalogs"],
        desc=f"{output.name}: index models",
        unit="entry",
        mininterval=2,
    ):
        identity = (entry["path"], entry["object"])
        if identity in seen:
            continue
        seen.add(identity)
        oid, category = entry["object"], entry["category"]
        description = {"source_path": entry["path"], "object": oid}
        if oid in node_map:
            graph, node = node_map[oid]
            description.update(graph=planned["graphs"][graph]["file"], root_node=node)
            description.update(planned["model_bindings"].get(oid, {}))
        elif oid in planned["objects"]:
            description.update(planned["objects"][oid][2])
        else:
            continue
        if category == "textures":
            by_category[category].append(description)
            continue
        basename = re.sub(r"[^a-zA-Z0-9_.-]", "_", PurePosixPath(entry["path"]).name)
        filename = f"models/{category}/{basename}-{oid}.json"
        write_json(output / filename, description)
        by_category[category].append(
            {"source_path": entry["path"], "file": filename, "object": oid}
        )
    for category, entries in by_category.items():
        folder = "textures" if category == "textures" else f"models/{category}"
        write_json(
            output / folder / "index.json",
            sorted(entries, key=lambda row: (row["source_path"], row["object"])),
        )
    write_json(
        output / "models/index.json",
        {
            category: f"models/{category}/index.json"
            for category in sorted(by_category)
            if category != "textures"
        },
    )
    scenes = [graph for graph in planned["graphs"] if graph["asset"]["scene"]]
    write_json(
        output / "areas/index.json",
        {
            "scenes": scenes,
            "unavailable_scenes": planned["coverage"]["unavailable_scenes"],
        },
    )


def extract_build(
    build: str,
    entry: dict[str, Any],
    cache: Path,
    output: Path,
    work: Path,
    conversion_cache: Path = Path("conversions"),
) -> dict[str, Any]:
    if output.exists():
        shutil.rmtree(output)
    output.mkdir(parents=True)
    scanner = Scanner(cache, work, output)
    metadata = scanner.run()
    planned = plan(metadata, work, output)
    exported = export_assets(
        planned.pop("selection"), scanner.reader, output, conversion_cache
    )
    for oid, result in exported.items():
        planned["objects"][oid][2].update(result)
    write_catalogs(planned, output)
    materials = {
        oid: value for oid, value in planned["objects"].items() if value[0] == 21
    }
    resources = {
        oid: value for oid, value in planned["objects"].items() if value[0] != 21
    }
    (output / "metadata/materials.msgpack").write_bytes(pack(materials))
    (output / "metadata/resources.msgpack").write_bytes(pack(resources))
    (output / "metadata/sources.msgpack").write_bytes(
        pack({"assets": metadata["assets"], "keys": metadata["keys"]})
    )
    write_json(output / "metadata/graphs.json", planned["graphs"])
    manifest = {
        "format_version": FORMAT_VERSION,
        "build": build,
        "revision": entry["revision"],
        "nickname": entry.get("nickname"),
        "capabilities": {
            "geometry": "unquantized GLB",
            "textures": "lossless WebP",
            "terrain": True,
            "skin_source_data": True,
            "animations": True,
            "animation_encoding": "Unity legacy source curves",
            "rig_bindings": True,
        },
        "coordinates": COORDINATES,
        "models": "models/index.json",
        "areas": "areas/index.json",
        "animations": "animations/index.json",
        "rigs": "models/rigs.msgpack",
        "materials": "metadata/materials.msgpack",
        "resources": "metadata/resources.msgpack",
        "graphs": "metadata/graphs.json",
        "node_map": "metadata/node-map.bin",
        "source_objects": "metadata/sources.msgpack",
        "coverage": "metadata/coverage.json",
    }
    cache_manifest = cache / ".cache-manifest.json"
    if cache_manifest.exists():
        version = json.loads(cache_manifest.read_text())
        manifest["cache_uuid"] = version.get("uuid")
        write_json(output / "metadata/cache-manifest.json", version)
        cache_files = cache / ".cache-files.json"
        if cache_files.exists():
            write_json(
                output / "metadata/cache-files.json",
                json.loads(cache_files.read_text()),
            )
    write_json(output / "manifest.json", manifest)
    (output / "README.md").write_text(
        "# FusionFall Object Pack\n\n"
        "Start with manifest.json and the models/ and areas/ indexes.\n"
        "Use animations/ for source clips and bindings, models/rigs.msgpack for clothing maps, and models/skins/ for weights and bind poses.\n"
        "Format: https://github.com/FinnHornhoover/FFObjectPacks/blob/main/FORMAT.md\n"
    )
    return manifest
