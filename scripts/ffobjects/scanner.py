from __future__ import annotations

import collections
import gc
import logging
from pathlib import Path
from typing import Any

from unitypack.object import ObjectInfo, ObjectPointer

from .cache import CacheReader
from .common import content_file, pack, write_json
from .terrain import encode_details, encode_heights

VISUAL_TYPES = {1, 4, 21, 23, 26, 27, 28, 33, 43, 48, 89, 104, 108, 137, 154, 156, 157}
COMPONENT_TYPES = {4, 23, 26, 33, 108, 137, 154}


class Scanner:
    def __init__(self, root: Path, work: Path, output: Path) -> None:
        self.reader = CacheReader(root)
        self.work, self.output = work, output
        self.keys: list[tuple[str, int]] = []
        self.keymap: dict[tuple[str, int], int] = {}
        self.assets: list[dict[str, Any]] = []
        self.errors: list[dict[str, Any]] = []
        self.prefabs: list[list[Any]] = []
        self.scenes: list[dict[str, Any]] = []
        self.terrains: list[int] = []
        self.counts: collections.Counter[str] = collections.Counter()

    def oid(self, asset: str, path_id: int) -> int:
        key = (asset.lower(), path_id)
        if key not in self.keymap:
            self.keymap[key] = len(self.keys)
            self.keys.append(key)
        return self.keymap[key]

    def pointer(self, pointer: ObjectPointer | None) -> int | None:
        if not pointer:
            return None
        try:
            name = (
                pointer.source_asset.asset_refs[pointer.file_id].file_path
                if pointer.file_id
                else pointer.source_asset.name
            )
        except (IndexError, KeyError):
            self.errors.append(
                {
                    "kind": "bad-reference",
                    "asset": pointer.source_asset.name,
                    "file_id": pointer.file_id,
                    "path_id": pointer.path_id,
                }
            )
            name = f"unresolved:{pointer.source_asset.name}:{pointer.file_id}"
        return self.oid(name, pointer.path_id)

    def convert(self, value: Any) -> Any:
        if isinstance(value, ObjectPointer):
            return {"r": self.pointer(value)}
        value = getattr(value, "_obj", value)
        if isinstance(value, dict):
            return {key: self.convert(child) for key, child in value.items()}
        if isinstance(value, (tuple, list)):
            return [self.convert(child) for child in value]
        return value

    def terrain(self, raw: dict[str, Any]) -> dict[str, Any]:
        data = raw.copy()
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
        return self.convert(data)

    def describe(self, obj: ObjectInfo, scene: bool) -> dict[str, Any]:
        tid = obj.type_id
        raw = obj.read()
        raw = getattr(raw, "_obj", raw)
        if tid == 43:
            return {"name": raw.get("m_Name", "")}
        if tid in (27, 28, 89):
            return {
                "name": raw.get("m_Name", ""),
                "width": raw.get("m_Width"),
                "height": raw.get("m_Height"),
                "format": raw.get("m_TextureFormat"),
                "settings": self.convert(raw.get("m_TextureSettings", {})),
            }
        if tid == 1:
            return {
                "name": raw.get("m_Name", ""),
                "active": raw.get("m_IsActive", 1),
                "layer": raw.get("m_Layer", 0),
                "components": [
                    [kind, self.pointer(pointer)]
                    for kind, pointer in raw["m_Component"]
                    if kind in COMPONENT_TYPES or (scene and kind < 0)
                ],
            }
        if tid == 4:
            return {
                "go": self.pointer(raw["m_GameObject"]),
                "parent": self.pointer(raw["m_Father"]),
                "p": [raw["m_LocalPosition"][axis] for axis in "xyz"],
                "q": [raw["m_LocalRotation"][axis] for axis in "xyzw"],
                "s": [raw["m_LocalScale"][axis] for axis in "xyz"],
            }
        if tid == 33:
            return {
                "go": self.pointer(raw["m_GameObject"]),
                "mesh": self.pointer(raw["m_Mesh"]),
            }
        if tid == 48:
            return {"name": raw.get("m_Name", "")}
        if tid == 156:
            return self.terrain(raw)
        return self.convert(raw)

    def run(self) -> dict[str, Any]:
        self.work.mkdir(parents=True, exist_ok=True)
        logging.getLogger("unitypack").setLevel(logging.ERROR)
        with (self.work / "nodes.msgpack").open("wb") as stream:
            for path in self.reader.files():
                # Download everything; audio-only bundles have no preview dependencies.
                if "music" in path.name.lower() or "fusound" in path.name.lower():
                    continue
                source = path.relative_to(self.reader.root).as_posix()
                try:
                    for asset, version in self.reader.assets(path):
                        aid = len(self.assets)
                        name = asset.name.lower()
                        scene = name.startswith("buildplayer-") and not name.endswith(
                            ".sharedassets"
                        )
                        self.assets.append(
                            {
                                "name": name,
                                "source": source,
                                "scene": scene,
                                "version": version,
                                "format": asset.format,
                            }
                        )
                        scene_roots = []
                        for obj in asset.objects.values():
                            tid = obj.type_id
                            if tid == 142:
                                for source_path, entry in obj.read()["m_Container"]:
                                    # Keep names and aliases, including independent clothing texture variants.
                                    self.prefabs.append(
                                        [source_path, self.pointer(entry["asset"])]
                                    )
                                continue
                            if tid not in VISUAL_TYPES:
                                if scene and tid < 0:
                                    try:
                                        raw = obj.read()
                                        raw = getattr(raw, "_obj", raw)
                                        if any("terrain" in key.lower() for key in raw):
                                            oid = self.oid(name, obj.path_id)
                                            stream.write(
                                                pack([oid, tid, aid, self.convert(raw)])
                                            )
                                            scene_roots.append(oid)
                                    except Exception:
                                        self.counts["unsupported_scene_scripts"] += 1
                                continue
                            oid = self.oid(name, obj.path_id)
                            try:
                                data = self.describe(obj, scene)
                                stream.write(pack([oid, tid, aid, data]))
                                self.counts[str(tid)] += 1
                                if tid == 156:
                                    self.terrains.append(oid)
                                if scene and tid in (23, 26, 104, 108, 137, 154, 157):
                                    scene_roots.append(oid)
                            except Exception as error:
                                self.errors.append(
                                    {
                                        "kind": "object",
                                        "file": source,
                                        "asset": name,
                                        "path_id": obj.path_id,
                                        "type": tid,
                                        "error": str(error),
                                    }
                                )
                        if scene_roots:
                            self.scenes.append({"asset": aid, "roots": scene_roots})
                        del asset
                        gc.collect()
                except Exception as error:
                    # Bundle failures cannot be treated as a successful, complete extraction.
                    raise RuntimeError(f"Cannot scan {path.name}: {error}") from error
                print(f"Scanned {path.name}: {len(self.assets)} assets", flush=True)
        metadata = {
            "assets": self.assets,
            "keys": self.keys,
            "scene_roots": self.scenes,
            "terrain_roots": self.terrains,
            "prefab_roots": self.prefabs,
            "errors": self.errors,
            "counts": dict(self.counts),
        }
        (self.work / "scan.msgpack").write_bytes(pack(metadata))
        write_json(
            self.work / "scan-summary.json",
            {
                k: v
                for k, v in metadata.items()
                if k not in ("keys", "prefab_roots", "scene_roots")
            },
        )
        return metadata
