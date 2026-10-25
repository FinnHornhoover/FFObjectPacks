from __future__ import annotations

import hashlib
import json
import os
import tempfile
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import msgpack
import yaml


def load_config(path: Path) -> dict[str, dict[str, Any]]:
    return yaml.safe_load(path.read_text())["config"]


def zip_name(build: str, entry: dict[str, Any]) -> str:
    nickname = f"_{entry['nickname']}" if entry.get("nickname") else ""
    return f"{build}_r{entry['revision']}{nickname}.zip"


def write_json(path: Path, data: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n")


def pack(data: Any) -> bytes:
    return msgpack.packb(data, use_bin_type=True)


def unpack(data: bytes) -> Any:
    return msgpack.unpackb(data, raw=False, strict_map_key=False)


def content_file(root: Path, folder: str, data: bytes, extension: str) -> str:
    digest = hashlib.sha256(data).hexdigest()
    relative = f"{folder}/{digest}{extension}"
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    if not path.exists():
        with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as stream:
            temporary = Path(stream.name)
            try:
                stream.write(data)
                os.fchmod(stream.fileno(), 0o644)
                stream.close()
                os.replace(temporary, path)
            finally:
                temporary.unlink(missing_ok=True)
    return relative


def refs(value: Any) -> Iterator[int]:
    if isinstance(value, dict):
        if set(value) == {"r"}:
            if value["r"] is not None:
                yield value["r"]
        else:
            for key, child in value.items():
                if key not in ("m_Script", "m_DefaultPhysicMaterial"):
                    yield from refs(child)
    elif isinstance(value, (list, tuple)):
        for child in value:
            yield from refs(child)


def rawref(value: dict[str, Any], key: str) -> int | None:
    child = value.get(key)
    return child.get("r") if isinstance(child, dict) else None
