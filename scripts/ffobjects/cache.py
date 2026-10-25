from __future__ import annotations

import io
import json
import logging
import lzma
from collections.abc import Iterator
from pathlib import Path
from typing import Any

from unitypack.asset import Asset
from unitypack.object import ObjectInfo, ObjectPointer
from unitypack.utils import BinaryReader

from .common import pack, unpack
from .native import read_object


def configure_unitypack() -> None:
    if getattr(ObjectPointer, "_ffobjects_compatibility", False):
        return

    def filter_manager_warnings(record: logging.LogRecord) -> bool:
        return not (
            record.levelno == logging.WARNING
            and record.module == "asset"
            and record.msg == "%r absent from structs.dat"
            and record.args in ((7,), (31,), (35,), (46,), (63,), (127,))
        )

    logging.getLogger().addFilter(filter_manager_warnings)
    original_load = ObjectPointer.load

    def load(pointer: ObjectPointer, reader: BinaryReader) -> None:
        if pointer.source_asset.format != 7:
            return original_load(pointer, reader)
        first = reader.read_uint()
        pointer.file_id = first & 0xFFFF
        pointer.path_id = ((first >> 16) << 32) | reader.read_uint()

    # UnityPack truncates object IDs and misreads format-7 pointer file IDs.
    ObjectInfo._read = read_object
    ObjectInfo.epath_id = property(lambda obj: obj.path_id)
    ObjectPointer.load = load
    ObjectPointer._ffobjects_compatibility = True


class NonBundleError(ValueError):
    pass


def members(path: Path) -> Iterator[tuple[str, io.BytesIO, str]]:
    with path.open("rb") as stream:
        prefix = stream.read(16)
        if not prefix.startswith((b"Unity", b"streamed\0")):
            raise NonBundleError(f"Not a Unity bundle: {path.name}")
        stream.seek(0)
        header = BinaryReader(stream, endian=">")
        signature = header.read_string()
        if signature not in ("UnityWeb", "UnityRaw", "streamed"):
            raise ValueError(f"Unsupported Unity bundle signature: {signature!r}")
        header.read_int()
        header.read_string()
        version = header.read_string()
        header.read_uint()
        header_size = header.read_int()
        if not 0 < header_size <= path.stat().st_size:
            raise ValueError("Invalid bundle header size")
        stream.seek(header_size)
        payload = stream.read()
    if signature in ("UnityWeb", "streamed"):
        payload = lzma.decompress(payload)
    reader = BinaryReader(io.BytesIO(payload), endian=">")
    count = reader.read_int()
    if not 0 < count < 100000:
        raise ValueError(f"Invalid member count: {count}")
    entries = [
        (reader.read_string(), reader.read_uint(), reader.read_uint())
        for _ in range(count)
    ]
    for name, offset, size in entries:
        if offset < reader.tell() or offset + size > len(payload):
            raise ValueError(f"Invalid member bounds: {name}")
        # Some original UnityRaw bundles have trailing bytes; member bounds are authoritative.
        buffer = io.BytesIO(payload[offset : offset + size])
        buffer.name = name
        yield name, buffer, version


class CacheReader:
    def __init__(
        self,
        root: Path,
        spool: Path | None = None,
        fallback: dict[int, dict[int, Any]] | None = None,
    ) -> None:
        configure_unitypack()
        self.root, self.spool = root, spool
        if fallback is not None:
            self.fallback = fallback
            return
        self.fallback: dict[int, dict[int, Any]] = {}
        available = {path.relative_to(root).as_posix() for path in self.files()}
        # Original-client scenes omit their trees. Use trees from this same cache,
        # never from another client generation. Both known sources are needed.
        for name in (
            "Tutorial.resourceFile",
            "TrainingGrounds.resourceFile",
            "Map_15_12.unity3d",
        ):
            path = root / name
            if name not in available:
                continue
            for member, buffer, version in self.buffers(path):
                if member.lower().endswith(".dll"):
                    continue
                asset = Asset.from_file(buffer)
                asset.objects
                self.fallback.setdefault(asset.format, {}).update(asset.tree.type_trees)
        # Legacy formats 6 and 7 share schemas; pointer decoding uses the asset format.
        for tid, tree in self.fallback.get(7, {}).items():
            self.fallback.setdefault(6, {}).setdefault(tid, tree)

    def buffers(self, path: Path) -> Iterator[tuple[str, io.BytesIO, str]]:
        if self.spool is None:
            yield from members(path)
            return
        directory = self.spool / path.relative_to(self.root)
        index = directory / "index.msgpack"
        if index.exists():
            for name, filename, version in unpack(index.read_bytes()):
                buffer = io.BytesIO((directory / filename).read_bytes())
                buffer.name = name
                yield name, buffer, version
            return
        directory.mkdir(parents=True, exist_ok=True)
        entries = []
        for name, buffer, version in members(path):
            if name.lower().endswith(".dll"):
                continue
            filename = f"{len(entries)}.bin"
            (directory / filename).write_bytes(buffer.getbuffer())
            entries.append([name, filename, version])
            yield name, buffer, version
        index.write_bytes(pack(entries))

    def assets(self, path: Path) -> Iterator[tuple[Asset, str]]:
        for name, buffer, version in self.buffers(path):
            if name.lower().endswith(".dll"):
                continue
            asset = Asset.from_file(buffer)
            asset.objects
            if not asset.tree.type_trees:
                asset.types.update(self.fallback.get(asset.format, {}))
            yield asset, version

    def files(self) -> list[Path]:
        snapshot = self.root / ".cache-files.json"
        if snapshot.exists():
            paths = []
            for name in json.loads(snapshot.read_text()):
                relative = Path(name)
                if relative.is_absolute() or ".." in relative.parts:
                    raise ValueError("Invalid cache snapshot path")
                path = self.root / relative
                if not path.is_file():
                    raise FileNotFoundError(f"Missing cache snapshot file: {name}")
                paths.append(path)
        else:
            paths = [path for path in self.root.rglob("*") if path.is_file()]
        return sorted(
            path for path in paths if path.suffix in (".resourceFile", ".unity3d")
        )
