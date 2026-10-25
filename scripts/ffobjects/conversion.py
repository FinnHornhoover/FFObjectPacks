from __future__ import annotations

import collections
import fcntl
import hashlib
import os
import shutil
import struct
from pathlib import Path
from typing import Any

from unitypack.engine.texture import Texture2D
from unitypack.object import ObjectInfo

from . import FORMAT_VERSION
from .cache import CacheReader
from .common import content_file, pack, unpack
from .geometry import image_webp, mesh_glb, texture_image
from .native import read_object

GEOMETRY_FIELDS = frozenset(
    {
        "m_MeshCompression",
        "m_CompressedMesh",
        "m_Vertices",
        "m_Normals",
        "m_UV",
        "m_UV1",
        "m_UV2",
        "m_UV3",
        "m_Colors",
        "m_Tangents",
        "m_SubMeshes",
        "m_Use16BitIndices",
        "m_IndexBuffer",
        "m_Skin",
        "m_BindPose",
    }
)

_reader: CacheReader
_output: Path
_store: Path


def initialize_worker(
    root: Path,
    spool: Path | None,
    fallback: dict[int, dict[int, Any]],
    output: Path,
    store: Path,
) -> None:
    global _reader, _output, _store
    _reader = CacheReader(root, spool, fallback)
    _output, _store = output, store / str(FORMAT_VERSION)
    _store.mkdir(parents=True, exist_ok=True)


def materialize(result: dict[str, Any]) -> None:
    for field in ("file", "skin_data"):
        if field not in result:
            continue
        relative = result[field]
        target = _output / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        try:
            os.link(_store / relative, target)
        except FileExistsError:
            pass
        except OSError:
            shutil.copyfile(_store / relative, target)


def geometry_key(raw: dict[str, Any]) -> dict[str, Any]:
    data = {key: raw[key] for key in sorted(GEOMETRY_FIELDS) if key in raw}
    compressed = data["m_MeshCompression"] = bool(raw.get("m_MeshCompression", 0))
    data["m_SubMeshes"] = [
        {
            "firstByte": submesh["firstByte"],
            "indexCount": submesh["indexCount"],
            "isTriStrip": bool(submesh["isTriStrip"]),
        }
        for submesh in raw.get("m_SubMeshes", [])
    ]
    channels = {}
    floats = {"m_Vertices", "m_UV", "m_Normals"}
    decoded = floats | {"m_Triangles", "m_NormalSigns"}
    for name, channel in raw.get("m_CompressedMesh", {}).items():
        count = channel.get("m_NumItems", 0)
        if not count or (name in decoded and not compressed):
            continue
        if name not in decoded:
            channels[name] = (
                channel  # These channels are retained verbatim as skin data.
            )
            continue
        bits = channel["m_BitSize"]
        payload = channel["m_Data"][: (count * bits + 7) // 8]
        if count * bits % 8:
            payload = payload[:-1] + bytes(
                [payload[-1] & ((1 << (count * bits % 8)) - 1)]
            )
        channels[name] = {"m_NumItems": count, "m_BitSize": bits, "m_Data": payload}
        if name in floats:
            channels[name].update(
                m_Range=channel.get("m_Range", 0) if bits else 0,
                m_Start=channel.get("m_Start", 0),
            )
    if compressed:
        channels.setdefault("m_Vertices", {})
    data["m_CompressedMesh"] = channels
    ignored = (
        (
            "m_Vertices",
            "m_Normals",
            "m_UV",
            "m_UV1",
            "m_UV2",
            "m_UV3",
            "m_IndexBuffer",
            "m_Use16BitIndices",
        )
        if compressed
        else ()
    )
    for key in ignored:
        data.pop(key, None)
    return data


def convert_asset(
    record: dict[str, Any], obj: ObjectInfo
) -> tuple[dict[str, Any], bool]:
    mesh = record["type"] == 43
    if mesh:
        raw = read_object(obj, GEOMETRY_FIELDS, wrap=False)
        raw = geometry_key(raw)
        digest = hashlib.sha256(pack(raw)).hexdigest()
    else:
        value = obj.read()
        if record["type"] == 27:
            value = Texture2D(getattr(value, "_obj", value))
        image = texture_image(value)
        digest = hashlib.sha256(
            struct.pack("<II", *image.size) + image.tobytes()
        ).hexdigest()
    directory = _store / ("mesh-keys" if mesh else "texture-keys") / digest[:2]
    directory.mkdir(parents=True, exist_ok=True)
    metadata = directory / f"{digest}.msgpack"
    with (directory / f"{digest}.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        reused = metadata.exists()
        if reused:
            result = unpack(metadata.read_bytes())
        else:
            if mesh:
                data, auxiliary, info = mesh_glb(raw)
                result = {
                    **info,
                    "file": content_file(_store, "models/geometry", data, ".glb"),
                }
                if auxiliary:
                    result["skin_data"] = content_file(
                        _store, "models/skins", auxiliary, ".msgpack"
                    )
            else:
                result = {
                    "file": content_file(
                        _store, "textures", image_webp(image), ".webp"
                    ),
                    "dimensions": list(image.size),
                    "rgba_exact": True,
                }
            metadata.write_bytes(pack(result))
    materialize(result)
    return result, reused


def convert_bundle(
    filename: str, records: list[dict[str, Any]]
) -> tuple[dict[int, dict[str, Any]], list[dict[str, Any]], int]:
    wanted = collections.defaultdict(list)
    for record in records:
        wanted[record["asset"]].append(record)
    results, errors, reused = {}, [], 0
    for asset, _ in _reader.assets(_reader.root / filename):
        for record in wanted.get(asset.name.lower(), []):
            try:
                result, hit = convert_asset(record, asset.objects[record["path_id"]])
                results[record["object"]] = result
                reused += hit
            except Exception as error:
                errors.append({**record, "error": str(error)})
    return results, errors, reused
