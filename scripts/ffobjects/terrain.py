from __future__ import annotations

import struct
import zlib
from typing import Any

import numpy as np
from numpy.typing import NDArray

from .common import pack, unpack


def encode_heights(heightmap: dict[str, Any]) -> bytes:
    values = heightmap["m_Heights"]
    width, height = heightmap["m_Width"], heightmap["m_Height"]
    signed = bool(values and min(values) < 0)
    original = np.asarray(values, dtype="<i2" if signed else "<u2")
    if original.size != width * height or original.tolist() != list(values):
        raise ValueError("Invalid 16-bit heightfield")
    matrix = original.view("<u2").reshape(height, width)
    previous = np.zeros_like(matrix)
    previous[:, 1:] = matrix[:, :-1]
    delta = (matrix.astype("u4") - previous.astype("u4")).astype("<u2")
    xor = np.bitwise_xor(matrix, previous).astype("<u2")
    header = struct.pack("<III", width, height, signed)
    choices = [
        magic + header + data.tobytes()
        for magic, data in ((b"HMP1", matrix), (b"HMD1", delta), (b"HMX1", xor))
    ]
    encoded = min(choices, key=lambda data: len(zlib.compress(data, 9)))
    if decode_heights(encoded).tolist() != list(values):
        raise ValueError("Heightfield roundtrip failed")
    return encoded


def decode_heights(data: bytes) -> NDArray[np.int16 | np.uint16]:
    width, height, signed = struct.unpack_from("<III", data, 4)
    values = np.frombuffer(data[16:], dtype="<u2").reshape(height, width)
    if data[:4] == b"HMD1":
        values = np.cumsum(values, axis=1, dtype="u4").astype("<u2")
    elif data[:4] == b"HMX1":
        values = np.bitwise_xor.accumulate(values, axis=1).astype("<u2")
    elif data[:4] != b"HMP1":
        raise ValueError("Unknown heightfield encoding")
    return values.view("<i2" if signed else "<u2").reshape(-1)


def sparse_counts(data: bytes) -> bytes:
    array = np.frombuffer(data, dtype="u1")
    indices = np.flatnonzero(array)
    gaps = np.diff(np.concatenate((np.array([-1], dtype="i8"), indices)))
    output = bytearray()
    for gap, value in zip(gaps, array[indices]):
        gap = int(gap)
        while gap >= 128:
            output.append((gap & 127) | 128)
            gap >>= 7
        output.extend((gap, int(value)))
    return bytes(output)


def unsparse_counts(data: bytes, length: int) -> bytes:
    output = bytearray(length)
    cursor, index = 0, -1
    while cursor < len(data):
        gap, shift = 0, 0
        while True:
            value = data[cursor]
            cursor += 1
            gap |= (value & 127) << shift
            if value < 128:
                break
            shift += 7
            if shift > 63:
                raise ValueError("Invalid sparse count gap")
        index += gap
        if not 0 <= index < length:
            raise ValueError("Invalid sparse count index")
        output[index] = data[cursor]
        cursor += 1
    return bytes(output)


def patch_bytes(patch: dict[str, Any]) -> bytes:
    bounds = [
        patch["bounds"][key][axis] for key in ("m_Center", "m_Extent") for axis in "xyz"
    ]
    layers, counts = bytes(patch["layerIndices"]), bytes(patch["numberOfObjects"])
    return struct.pack("<6fII", *bounds, len(layers), len(counts)) + layers + counts


def encode_details(patches: list[dict[str, Any]]) -> bytes:
    records = [patch_bytes(patch) for patch in patches]
    lengths = [struct.unpack_from("<II", record, 24) for record in records]
    if not records or len(set(lengths)) > 1:
        palette, lookup, indices = [], {}, []
        for record in records:
            if record not in lookup:
                lookup[record] = len(palette)
                palette.append(record)
            indices.append(lookup[record])
        data = pack({"mode": "local-palette", "records": palette, "indices": indices})
    else:
        layer_bytes, count_bytes = lengths[0]
        counts = b"".join(record[32 + layer_bytes :] for record in records)
        sparse = sparse_counts(counts)
        use_sparse = len(zlib.compress(sparse, 9)) < len(zlib.compress(counts, 9))
        data = pack(
            {
                "mode": "sparse-soa",
                "patches": len(records),
                "layer_bytes": layer_bytes,
                "count_bytes": count_bytes,
                "bounds": b"".join(record[:24] for record in records),
                "layer_indices": b"".join(
                    record[32 : 32 + layer_bytes] for record in records
                ),
                "count_encoding": "delta-nonzero" if use_sparse else "dense",
                "count_length": len(counts),
                "counts": sparse if use_sparse else counts,
            }
        )
    if decode_details(data) != records:
        raise ValueError("Detail patch roundtrip failed")
    return data


def decode_details(data: bytes) -> list[bytes]:
    record = unpack(data)
    if record["mode"] == "local-palette":
        return [record["records"][index] for index in record["indices"]]
    counts = record["counts"]
    if record["count_encoding"] == "delta-nonzero":
        counts = unsparse_counts(counts, record["count_length"])
    nl, nc = record["layer_bytes"], record["count_bytes"]
    return [
        record["bounds"][i * 24 : (i + 1) * 24]
        + struct.pack("<II", nl, nc)
        + record["layer_indices"][i * nl : (i + 1) * nl]
        + counts[i * nc : (i + 1) * nc]
        for i in range(record["patches"])
    ]
