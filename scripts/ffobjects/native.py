from __future__ import annotations

from typing import Any

from unitypack import engine
from unitypack.object import ObjectInfo, ObjectPointer

from . import _native


def read_object(
    obj: ObjectInfo, fields: frozenset[str] | None = None, wrap: bool = True
) -> Any:
    asset = obj.asset
    schemas = getattr(asset, "_ffobjects_schemas", None)
    if schemas is None:
        asset._ffobjects_schemas = schemas = {}
    tree = obj.type_tree
    if tree not in schemas:
        schemas[tree] = _native.compile(tree, engine)
    asset._buf.seek(asset._buf_ofs + obj.data_offset)
    return _native.decode(
        schemas[tree],
        asset._buf.read(obj.size),
        asset,
        ObjectPointer,
        obj,
        asset.format,
        fields,
        wrap,
    )
