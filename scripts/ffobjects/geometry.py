from __future__ import annotations

import io
import json
import struct
from typing import Any

import numpy as np
from numpy.typing import NDArray
from PIL import Image, ImageOps
from unitypack.engine.mesh import Mesh
from unitypack.engine.texture import Texture2D
from unitypack.utils import BitReader

from .common import pack


def plain(value: Any) -> Any:
    value = getattr(value, "_obj", value)
    if isinstance(value, dict):
        return {key: plain(child) for key, child in value.items()}
    if isinstance(value, (tuple, list)):
        return [plain(child) for child in value]
    return value


def packed_ints(value: dict[str, Any]) -> list[int]:
    count = value.get("m_NumItems", 0)
    if not count:
        return []
    if not value["m_BitSize"]:
        return [0] * count
    reader = BitReader(value["m_Data"], value["m_BitSize"])
    return [reader.read() for _ in range(count)]


def packed_floats(value: dict[str, Any]) -> list[float]:
    integers = packed_ints(value)
    bits = value.get("m_BitSize", 0)
    scale = value.get("m_Range", 0) / ((1 << bits) - 1) if bits else 0
    return [number * scale + value.get("m_Start", 0) for number in integers]


def triangles(indices: list[int], strip: bool) -> list[int]:
    if not strip:
        if len(indices) % 3:
            raise ValueError("Triangle list has a partial triangle")
        return list(indices)
    output = []
    for index in range(2, len(indices)):
        face = indices[index - 2 : index + 1]
        if len(set(face)) == 3:
            output.extend(face if index % 2 == 0 else face[::-1])
    return output


def encode_glb(document: dict[str, Any], binary: bytes = b"") -> bytes:
    data = json.dumps(document, separators=(",", ":"), allow_nan=False).encode()
    data += b" " * (-len(data) % 4)
    chunks = struct.pack("<I4s", len(data), b"JSON") + data
    if binary:
        binary += b"\0" * (-len(binary) % 4)
        chunks += struct.pack("<I4s", len(binary), b"BIN\0") + binary
    return b"glTF" + struct.pack("<II", 2, 12 + len(chunks)) + chunks


def mesh_glb(mesh: Mesh | dict[str, Any]) -> tuple[bytes, bytes | None, dict[str, Any]]:
    raw = mesh if isinstance(mesh, dict) else plain(mesh)
    compressed = bool(raw.get("m_MeshCompression", 0))
    channels = raw.get("m_CompressedMesh", {})
    if compressed:
        positions = np.asarray(
            packed_floats(channels["m_Vertices"]), dtype="<f4"
        ).reshape(-1, 3)
        xy = np.asarray(
            packed_floats(channels.get("m_Normals", {})), dtype="f8"
        ).reshape(-1, 2)
        signs = packed_ints(channels.get("m_NormalSigns", {}))
        if len(xy) != len(signs):
            raise ValueError("Compressed normal sign count differs")
        z = np.sqrt(np.maximum(0, 1 - np.sum(xy * xy, axis=1))) * np.where(signs, 1, -1)
        normals = np.column_stack((xy, z)).astype("<f4")
        uv = np.asarray(packed_floats(channels.get("m_UV", {})), dtype="<f4").reshape(
            -1, 2
        )
        packed_indices = packed_ints(channels.get("m_Triangles", {}))
    else:
        positions = np.asarray(
            [[v[k] for k in "xyz"] for v in raw.get("m_Vertices", [])], dtype="<f4"
        ).reshape(-1, 3)
        normals = np.asarray(
            [[v[k] for k in "xyz"] for v in raw.get("m_Normals", [])], dtype="<f4"
        ).reshape(-1, 3)
        uv = np.asarray(
            [[v[k] for k in "xy"] for v in raw.get("m_UV", [])], dtype="<f4"
        ).reshape(-1, 2)
        packed_indices = None
    count = len(positions)
    document = {
        "asset": {"version": "2.0", "generator": "FFObjectPacks"},
        "scene": 0,
        "scenes": [{"nodes": []}],
    }
    if not count:
        return (
            encode_glb(document),
            None,
            {"vertices": 0, "triangles": 0, "attributes": [], "source_empty": True},
        )
    if not np.isfinite(positions).all():
        raise ValueError("Nonfinite vertex position")
    positions[:, 0] *= -1
    if len(normals):
        normals[:, 0] *= -1
    binary, views, accessors, attributes = bytearray(), [], [], {}

    def add(
        array: NDArray[Any],
        kind: str,
        component: int = 5126,
        target: int = 34962,
        normalized: bool = False,
        bounds: bool = False,
    ) -> int:
        array = np.ascontiguousarray(array)
        data = array.tobytes()
        views.append(
            {
                "buffer": 0,
                "byteOffset": len(binary),
                "byteLength": len(data),
                "target": target,
            }
        )
        binary.extend(data)
        binary.extend(b"\0" * (-len(binary) % 4))
        accessor = {
            "bufferView": len(views) - 1,
            "componentType": component,
            "count": len(array),
            "type": kind,
        }
        if normalized:
            accessor["normalized"] = True
        if bounds:
            accessor.update(
                min=array.min(axis=0).tolist(), max=array.max(axis=0).tolist()
            )
        accessors.append(accessor)
        return len(accessors) - 1

    attributes["POSITION"] = add(positions, "VEC3", bounds=True)
    if len(normals):
        if len(normals) != count:
            raise ValueError("Normal count differs from vertex count")
        attributes["NORMAL"] = add(normals, "VEC3")
    if len(uv) % count:
        raise ValueError("UV channel has a partial vertex array")
    for index in range(len(uv) // count):
        attributes[f"TEXCOORD_{index}"] = add(
            uv[index * count : (index + 1) * count], "VEC2"
        )
    if not compressed:
        for index, key in enumerate(("m_UV1", "m_UV2", "m_UV3"), 1):
            values = raw.get(key, [])
            if values:
                if len(values) != count:
                    raise ValueError(f"{key} count differs from vertex count")
                attributes[f"TEXCOORD_{index}"] = add(
                    np.asarray([[v[k] for k in "xy"] for v in values], dtype="<f4"),
                    "VEC2",
                )
    colors = raw.get("m_Colors", [])
    if colors:
        if len(colors) != count:
            raise ValueError("Colour count differs from vertex count")
        values = [
            (
                [(v["rgba"] >> (8 * i)) & 255 for i in range(4)]
                if "rgba" in v
                else [v[k] for k in "rgba"]
            )
            for v in colors
        ]
        integers = all(isinstance(v, int) for color in values for v in color)
        attributes["COLOR_0"] = add(
            np.asarray(values, dtype="u1" if integers else "<f4"),
            "VEC4",
            5121 if integers else 5126,
            normalized=integers,
        )
    tangents = raw.get("m_Tangents", [])
    if tangents:
        if len(tangents) != count:
            raise ValueError("Tangent count differs from vertex count")
        values = np.asarray(
            [[-v["x"], v["y"], v["z"], -v["w"]] for v in tangents], dtype="<f4"
        )
        attributes["TANGENT"] = add(values, "VEC4")
    primitives, triangle_count = [], 0
    submeshes = raw.get("m_SubMeshes", [])
    for index, submesh in enumerate(submeshes):
        offset, length = submesh["firstByte"], submesh["indexCount"]
        if packed_indices is not None:
            indices = packed_indices[offset // 2 : offset // 2 + length]
        else:
            stride = 2 if raw.get("m_Use16BitIndices", True) else 4
            indices = np.frombuffer(
                raw["m_IndexBuffer"][offset : offset + length * stride],
                dtype="<u2" if stride == 2 else "<u4",
            ).tolist()
        if len(indices) != length:
            raise ValueError("Submesh index range exceeds buffer")
        faces = triangles(indices, bool(submesh["isTriStrip"]))
        if not faces:
            continue
        if min(faces) < 0 or max(faces) >= count:
            raise ValueError("Vertex index out of bounds")
        dtype, component = ("<u2", 5123) if count < 65536 else ("<u4", 5125)
        faces = np.asarray(faces, dtype=dtype).reshape(-1, 3)[:, ::-1].reshape(-1)
        primitives.append(
            {
                "attributes": attributes.copy(),
                "indices": add(faces, "SCALAR", component, 34963),
                "material": index,
            }
        )
        triangle_count += len(faces) // 3
    if not primitives:
        raise ValueError("Nonempty mesh contains no triangles")
    document.update(
        scenes=[{"nodes": [0]}],
        nodes=[{"mesh": 0}],
        meshes=[{"primitives": primitives}],
        materials=[{"name": f"slot_{i}"} for i in range(len(submeshes))],
        buffers=[{"byteLength": len(binary)}],
        bufferViews=views,
        accessors=accessors,
    )
    auxiliary = {key: raw[key] for key in ("m_Skin", "m_BindPose") if raw.get(key)}
    remaining = {
        key: value
        for key, value in channels.items()
        if key
        not in ("m_Vertices", "m_UV", "m_Triangles", "m_Normals", "m_NormalSigns")
        and value.get("m_NumItems", 0)
    }
    if remaining:
        auxiliary["compressed_channels"] = remaining
    metadata = {
        "vertices": count,
        "triangles": triangle_count,
        "attributes": list(attributes),
        "source_empty": False,
    }
    return (
        encode_glb(document, bytes(binary)),
        pack(auxiliary) if auxiliary else None,
        metadata,
    )


def texture_image(texture: Texture2D) -> Image.Image:
    image = texture.image
    if image is None:
        raise ValueError("Empty source texture")
    return ImageOps.flip(image).convert("RGBA")


def image_webp(image: Image.Image) -> bytes:
    output = io.BytesIO()
    image.save(output, format="WEBP", lossless=True, quality=80, method=1, exact=True)
    data = output.getvalue()
    if Image.open(io.BytesIO(data)).convert("RGBA").tobytes() != image.tobytes():
        raise ValueError("Lossless WebP roundtrip failed")
    return data


def texture_webp(texture: Texture2D) -> tuple[bytes, tuple[int, int]]:
    image = texture_image(texture)
    return image_webp(image), image.size
