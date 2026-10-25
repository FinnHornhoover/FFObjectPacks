from __future__ import annotations

import collections
from pathlib import Path
from typing import Any

from tqdm import tqdm

from .common import pack, rawref, write_json

RIG_FIELDS = {
    "ActorSkinCombiner": {"actorBones"},
    "ActorWearIndexTable": {"transformIndicesM", "transformIndicesF"},
    "ActorWearSkinTable": {"skins"},
}

NodeTable = dict[int, tuple[int, int, dict[str, Any]]]


def collect_bindings(
    nodes: NodeTable, selected: set[int], metadata: dict[str, Any], output: Path
) -> dict[str, Any]:
    clips = {oid: nodes[oid][2] for oid in sorted(selected) if nodes[oid][0] == 74}
    go_transforms = {}
    children = collections.defaultdict(lambda: collections.defaultdict(list))
    for oid in sorted(selected):
        tid, _, data = nodes[oid]
        if tid != 4:
            continue
        go = data["go"]
        if go is not None:
            go_transforms[go] = oid
        name = nodes[go][2].get("name", "") if go in nodes else ""
        children[data["parent"]][name].append(oid)

    def resolve(root: int | None, path: str) -> list[int]:
        matches = [root] if root is not None else []
        if path:
            for part in path.split("/"):
                matches = [
                    child
                    for parent in matches
                    for child in children[parent].get(part, [])
                ]
                if not matches:
                    break
        return matches

    bindings, rigs, model_bindings = {}, {}, {}
    used_clips = set()
    missing_clips, unresolved_paths, ambiguous_paths = [], [], []

    def link(oid: int, go: int | None, kind: str) -> None:
        transform = go_transforms.get(go)
        visited = set()
        while go is not None and go not in visited:
            visited.add(go)
            entry = model_bindings.setdefault(go, {"animations": [], "rigs": []})
            entry[kind].append(oid)
            if transform is None:
                break
            parent = nodes[transform][2]["parent"]
            if parent not in selected or nodes[parent][0] != 4:
                break
            transform = parent
            go = nodes[parent][2]["go"]

    components = [
        oid
        for oid in sorted(selected)
        if nodes[oid][0] == 111 or nodes[oid][2].get("script_type") in RIG_FIELDS
    ]
    for oid in tqdm(
        components,
        desc=f"{output.name}: bind animations and rigs",
        unit="component",
        mininterval=2,
    ):
        tid, _, data = nodes[oid]
        go = rawref(data, "m_GameObject")
        root = go_transforms.get(go)
        if tid != 111:
            rigs[oid] = {
                "script_type": data["script_type"],
                "game_object": go,
                "transform": root,
                "data": {
                    key: value for key, value in data.items() if key != "script_type"
                },
            }
            link(oid, go, "rigs")
            continue
        default = rawref(data, "m_Animation")
        references = [
            entry.get("r") if isinstance(entry, dict) else None
            for entry in data.get("m_Animations", [])
        ]
        clip_ids = list(
            dict.fromkeys(ref for ref in [*references, default] if ref is not None)
        )
        used_clips.update(clip_ids)
        missing = [ref for ref in clip_ids if ref not in clips]
        if missing:
            missing_clips.append({"binding": oid, "clips": missing})
        path_ids = sorted(
            {path for ref in clip_ids if ref in clips for path in clips[ref]["paths"]}
        )
        targets, unresolved, ambiguous = [], [], []
        for path_id in path_ids:
            matches = resolve(root, metadata["animation_paths"][path_id])
            if len(matches) == 1:
                targets.append([path_id, matches[0]])
            elif matches:
                ambiguous.append([path_id, matches])
            else:
                unresolved.append(path_id)
        if unresolved:
            unresolved_paths.append({"binding": oid, "paths": unresolved})
        if ambiguous:
            ambiguous_paths.append({"binding": oid, "paths": ambiguous})
        bindings[oid] = {
            "game_object": go,
            "transform": root,
            "default_clip": default,
            "clips": references,
            "settings": {
                key: value
                for key, value in data.items()
                if key not in ("m_GameObject", "m_Animation", "m_Animations")
            },
            "targets": targets,
            "unresolved_paths": unresolved,
            "ambiguous_paths": ambiguous,
        }
        link(oid, go, "animations")
    aliases = collections.defaultdict(list)
    for path, oid in metadata["prefab_roots"]:
        if oid in clips and path not in aliases[oid]:
            aliases[oid].append(path)
    write_json(
        output / "animations/index.json",
        {
            "paths": metadata["animation_paths"],
            "bindings": "animations/bindings.msgpack",
            "clips": [
                {
                    "object": oid,
                    "asset": nodes[oid][1],
                    **data,
                    "source_paths": aliases[oid],
                }
                for oid, data in clips.items()
            ],
        },
    )
    (output / "animations/bindings.msgpack").write_bytes(pack(bindings))
    (output / "models").mkdir(exist_ok=True)
    (output / "models/rigs.msgpack").write_bytes(pack(rigs))
    return {
        "bindings": bindings,
        "rigs": rigs,
        "model_bindings": model_bindings,
        "coverage": {
            "clips": len(clips),
            "bindings": len(bindings),
            "rigs": len(rigs),
            "unbound_clips": sorted(clips.keys() - used_clips),
            "missing_clips": missing_clips,
            "unresolved_paths": unresolved_paths,
            "ambiguous_paths": ambiguous_paths,
        },
    }
