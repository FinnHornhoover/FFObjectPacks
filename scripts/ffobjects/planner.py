from __future__ import annotations

import collections
import struct
from pathlib import Path, PurePosixPath
from typing import Any

import msgpack

from .common import pack, rawref, refs, write_json


def category(path: str) -> str:
    path = path.lower()
    if path.startswith("nano/"):
        return "nanos"
    if path.startswith("wear/"):
        return "items"
    if path.startswith("actor/"):
        return "players"
    if PurePosixPath(path).name.startswith("npc_"):
        return "npcs"
    if path.startswith("mob/") and PurePosixPath(path).name.startswith("mob_"):
        return "monsters"
    return "props"


def plan(metadata: dict[str, Any], work: Path, output: Path) -> dict[str, Any]:
    nodes = {}
    with (work / "nodes.msgpack").open("rb") as stream:
        for oid, tid, aid, data in msgpack.Unpacker(
            stream, raw=False, strict_map_key=False
        ):
            nodes[oid] = (tid, aid, data)
    children = collections.defaultdict(list)
    for oid, (tid, aid, data) in nodes.items():
        if tid == 4 and data["parent"] is not None:
            children[data["parent"]].append(oid)
    usable, unavailable = [], []
    for scene in metadata["scene_roots"]:
        missing_terrain = [
            oid
            for oid in scene["roots"]
            if oid in nodes
            and nodes[oid][0] == 154
            and rawref(nodes[oid][2], "m_TerrainData") not in nodes
        ]
        if missing_terrain:
            unavailable.append(
                {
                    "asset": metadata["assets"][scene["asset"]],
                    "reason": "Terrain bundles referenced by this scene are absent from the cache",
                    "colliders": missing_terrain,
                }
            )
        else:
            usable.append(scene)
    todo = [oid for scene in usable for oid in scene["roots"]] + metadata[
        "terrain_roots"
    ]
    expanded, catalogs = set(), []

    def add_tree(go: int | None) -> None:
        if go in expanded or go not in nodes or nodes[go][0] != 1:
            return
        expanded.add(go)
        stack = [oid for tid, oid in nodes[go][2]["components"] if tid == 4]
        visited = set()
        while stack:
            oid = stack.pop()
            if oid in visited:
                continue
            visited.add(oid)
            todo.append(oid)
            if oid in nodes:
                todo.append(nodes[oid][2]["go"])
            stack.extend(children.get(oid, []))

    for path, oid in metadata["prefab_roots"]:
        if oid not in nodes:
            continue  # Audio/animation/script containers are intentionally outside this extraction stage.
        tid = nodes[oid][0]
        if tid == 1:
            add_tree(oid)
            catalogs.append({"path": path, "object": oid, "category": category(path)})
        elif tid in (21, 27, 28, 43) and path.lower().startswith(
            ("texture/", "mob/", "nano/", "wear/", "actor/")
        ):
            todo.append(oid)
            catalogs.append(
                {
                    "path": path,
                    "object": oid,
                    "category": "textures" if tid in (27, 28) else category(path),
                }
            )
    seen, missing = set(), set()
    while todo:
        oid = todo.pop()
        if oid is None or oid in seen:
            continue
        seen.add(oid)
        if oid not in nodes:
            missing.add(oid)
            continue
        tid, aid, data = nodes[oid]
        if tid == 1:
            has_renderer = any(kind == 23 for kind, pointer in data["components"])
            todo.extend(
                pointer
                for kind, pointer in data["components"]
                if pointer in nodes and (kind != 33 or has_renderer)
            )
        elif tid == 4:
            todo.extend((data["parent"], data["go"]))
        elif tid == 33:
            todo.append(data["mesh"])
        elif tid in (23, 26, 137):
            todo.extend(refs(data))
            go = rawref(data, "m_GameObject")
            if tid == 23 and go in nodes:
                todo.extend(
                    pointer
                    for kind, pointer in nodes[go][2]["components"]
                    if kind == 33
                )
        elif tid == 154:
            todo.extend((rawref(data, "m_GameObject"), rawref(data, "m_TerrainData")))
        elif tid in (21, 104, 108, 156, 157) or tid < 0:
            dependencies = list(refs(data))
            todo.extend(dependencies)
            if tid == 156:
                for dependency in dependencies:
                    add_tree(dependency)
    selected = seen & nodes.keys()
    unresolved = [
        {"object": oid, "source_key": metadata["keys"][oid]} for oid in sorted(missing)
    ]
    unexpected = [
        entry
        for entry in unresolved
        if entry["source_key"][0] != "library/unity default resources"
    ]
    if unexpected:
        write_json(
            output / "metadata/coverage.json",
            {"missing": unresolved, "unavailable_scenes": unavailable},
        )
        raise ValueError(
            f"{len(unexpected)} unresolved visual dependencies; see metadata/coverage.json"
        )
    groups = collections.defaultdict(list)
    for oid in selected:
        groups[nodes[oid][1]].append(oid)
    graphs, node_maps, objects, selection = [], [], {}, []
    for aid, ids in sorted(groups.items()):
        transforms = sorted(oid for oid in ids if nodes[oid][0] == 4)
        transform_map = {oid: index for index, oid in enumerate(transforms)}
        names, name_map, binary, renderers, settings, external_parents = (
            [],
            {},
            bytearray(),
            [],
            [],
            [],
        )
        for oid in transforms:
            data = nodes[oid][2]
            go = data["go"]
            go_data = nodes[go][2] if go in nodes else {}
            name = go_data.get("name", "")
            if name not in name_map:
                name_map[name] = len(names)
                names.append(name)
            if data["parent"] is not None and data["parent"] not in transform_map:
                external_parents.append([oid, data["parent"]])
            flags = int(bool(go_data.get("active", 1))) | (
                int(go_data.get("layer", 0)) << 1
            )
            trs = data["p"] + data["q"] + data["s"]
            body = struct.pack(
                "<IiII10fI",
                oid,
                transform_map.get(data["parent"], -1),
                go if go is not None else 0xFFFFFFFF,
                name_map[name],
                *trs,
                flags,
            )
            if list(struct.unpack_from("<10f", body, 16)) != trs:
                raise ValueError("Transform precision changed")
            binary.extend(body)
            node_maps.append((oid, len(graphs), transform_map[oid]))
            if go is not None:
                node_maps.append((go, len(graphs), transform_map[oid]))
        for oid in sorted(ids):
            tid, _, data = nodes[oid]
            if tid in (1, 4, 33):
                continue
            if tid in (23, 26, 137, 154):
                go = rawref(data, "m_GameObject")
                components = nodes[go][2]["components"] if go in nodes else []
                transform = next(
                    (pointer for kind, pointer in components if kind == 4), None
                )
                if tid == 23:
                    mesh_filter = next(
                        (
                            pointer
                            for kind, pointer in components
                            if kind == 33 and pointer in nodes
                        ),
                        None,
                    )
                    mesh = (
                        nodes[mesh_filter][2]["mesh"] if mesh_filter in nodes else None
                    )
                else:
                    mesh = rawref(data, "m_Mesh")
                renderers.append(
                    [
                        oid,
                        tid,
                        transform,
                        mesh,
                        {
                            key: value
                            for key, value in data.items()
                            if key != "m_GameObject"
                        },
                    ]
                )
            elif tid in (104, 108, 157) or tid < 0:
                settings.append([oid, tid, data])
            else:
                objects[oid] = [tid, aid, data]
                if tid in (27, 28, 43, 89):
                    selection.append(
                        {
                            "object": oid,
                            "type": tid,
                            "source": metadata["assets"][aid]["source"],
                            "asset": metadata["assets"][aid]["name"],
                            "path_id": metadata["keys"][oid][1],
                        }
                    )
        if transforms or renderers or settings:
            asset = metadata["assets"][aid]
            folder = "areas/scenes" if asset["scene"] else "models/hierarchies"
            relative = f"{folder}/{aid}.msgpack"
            path = output / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(
                pack(
                    {
                        "asset": aid,
                        "names": names,
                        "node_stride": 60,
                        "nodes": bytes(binary),
                        "renderers": renderers,
                        "settings": settings,
                        "external_parents": external_parents,
                    }
                )
            )
            graphs.append(
                {
                    "file": relative,
                    "asset": asset,
                    "nodes": len(transforms),
                    "renderers": len(renderers),
                }
            )
    (output / "metadata").mkdir(exist_ok=True)
    unique_maps = sorted(set(node_maps))
    (output / "metadata/node-map.bin").write_bytes(
        b"NMAP"
        + struct.pack("<I", len(unique_maps))
        + b"".join(struct.pack("<IHI", *entry) for entry in unique_maps)
    )
    coverage = {
        "scenes": sum(graph["asset"]["scene"] for graph in graphs),
        "terrain_data": sum(nodes[oid][0] == 156 for oid in selected),
        "selected_types": dict(collections.Counter(nodes[oid][0] for oid in selected)),
        "missing": unresolved,
        "unavailable_scenes": unavailable,
        "scan_errors": metadata["errors"],
    }
    write_json(output / "metadata/coverage.json", coverage)
    return {
        "graphs": graphs,
        "objects": objects,
        "assets": metadata["assets"],
        "catalogs": catalogs,
        "selection": selection,
        "coverage": coverage,
    }
