from __future__ import annotations

import gc
import logging
from pathlib import Path
from typing import Any

from tqdm import tqdm
from unitypack.object import ObjectInfo, ObjectPointer

from . import _scan
from .cache import CacheReader, NonBundleError
from .common import content_file, pack, unpack, write_json
from .terrain import encode_details, encode_heights


class Scanner:
    def __init__(self, root: Path, work: Path, output: Path) -> None:
        self.work, self.output = work, output
        self.work.mkdir(parents=True, exist_ok=True)
        self.output.mkdir(parents=True, exist_ok=True)
        self.reader = CacheReader(root, work / "bundles")
        self.context = _scan.create(
            str(work / "nodes.msgpack"), self.terrain_payload, self.publish_clip
        )

    def convert(self, value: Any) -> Any:
        if isinstance(value, ObjectPointer):
            return {"r": _scan.pointer(self.context, value.file_id, value.path_id)}
        value = getattr(value, "_obj", value)
        if isinstance(value, dict):
            return {key: self.convert(child) for key, child in value.items()}
        if isinstance(value, (tuple, list)):
            return [self.convert(child) for child in value]
        return value

    def terrain_payload(self, obj: ObjectInfo) -> bytes:
        raw = obj.read()
        data = getattr(raw, "_obj", raw).copy()
        heights = data["m_Heightmap"].copy()
        encoded = encode_heights(heights)
        heights["m_Heights"] = {
            "file": content_file(self.output, "areas/terrain/heights", encoded, ".bin"),
            "format": encoded[:4].decode(),
        }
        data["m_Heightmap"] = heights
        details = data["m_DetailDatabase"].copy()
        details["m_Patches"] = {
            "file": content_file(
                self.output,
                "areas/terrain/details",
                encode_details(details["m_Patches"]),
                ".msgpack",
            )
        }
        data["m_DetailDatabase"] = details
        return pack(self.convert(data))

    def publish_clip(self, data: bytes) -> str:
        return content_file(self.output, "animations/clips", data, ".msgpack")

    def run(self) -> dict[str, Any]:
        logging.getLogger("unitypack").setLevel(logging.ERROR)
        try:
            with tqdm(
                self.reader.files(),
                desc=f"{self.reader.root.name}: scan",
                unit="file",
                mininterval=2,
            ) as progress:
                for path in progress:
                    progress.set_postfix_str(path.name, refresh=False)
                    if "music" in path.name.lower() or "fusound" in path.name.lower():
                        continue
                    source = path.relative_to(self.reader.root).as_posix()
                    try:
                        for asset, version in self.reader.assets(path):
                            _scan.scan_asset(self.context, asset, source, version)
                            del asset
                            gc.collect()
                    except NonBundleError:
                        _scan.non_bundle(self.context, source)
                    except Exception as error:
                        raise RuntimeError(
                            f"Cannot scan {path.name}: {error}"
                        ) from error
                    assets, clips, rigs, errors = _scan.stats(self.context)
                    progress.set_postfix(
                        assets=assets,
                        clips=clips,
                        rigs=rigs,
                        errors=errors,
                        refresh=False,
                    )
            summary = unpack(
                _scan.finish(self.context, str(self.work / "scan.msgpack"))
            )
        finally:
            self.context = None
        write_json(self.work / "scan-summary.json", summary)
        return summary
