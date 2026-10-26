# Pack Format 2

Read `manifest.json` for file paths and coordinate conventions. Paths are relative to the pack root. Metadata uses MessagePack; navigation indexes use JSON.

## References and Models

- References: `{"r": object_id}` or `{"r": null}`. IDs are local to the pack.
- `metadata/resources.msgpack` and `materials.msgpack`: ID → `[Unity class ID, source asset index, descriptor]`.
- `metadata/sources.msgpack`: original asset names and Unity IDs, indexed by pack-local ID.
- Model descriptors: source path, object ID, hierarchy `graph` and `root_node`.
- Texture index: original paths → shared WebP files, including clothing variants.

GLBs preserve vertex order, UV sets, colours and tangents. Renderer materials bind `slot_0`, `slot_1`, etc.; source shader properties live in the material table. Bind poses and weights are in `models/skins/`. Source animation clips are in `animations/`; geometry GLBs do not include glTF skins or animation tracks.

## Hierarchies

`metadata/graphs.json` lists MessagePack graphs containing `asset`, `names`, `nodes`, `renderers`, `settings`, `external_parents`, `animations` and `rigs`.

Each 60-byte node uses little-endian `<IiII10fI`: transform ID, local parent index (-1 for root), GameObject ID (0xffffffff for none), name index, position xyz, quaternion xyzw, scale xyz, flags. Flags store active in bit 0 and layer above it.

Renderers use `[component ID, class ID, transform ID, mesh ID, attributes]`. External parents use `[transform ID, parent transform ID]`.

`metadata/node-map.bin` starts with `NMAP` + uint32 count, followed by `<IHI>` rows: object ID, graph index, node index. Transform and GameObject IDs both map to nodes.

## Coordinates

Hierarchy, terrain and bind data retain Unity coordinates. GLB geometry reflects X and reverses winding; tangent X/W change sign. When assembling scenes, reflect position X and quaternion Y/Z, and conjugate bind matrices by `diag(-1,1,1,1)`.

UVs retain source values; WebP images are vertically flipped from Unity storage. Use `flipY=false` for glTF textures and set colour spaces according to texture role.

## Terrain

Heights: four-byte magic + `<III>` width, height, signed flag + row-major 16-bit samples. `HMP1` is raw; `HMD1` requires cumulative sums per row modulo 65536; `HMX1` requires cumulative XOR per row. Reinterpret as int16 when signed. Scale and layer settings remain in the terrain descriptor.

Detail MessagePack uses either:

- `local-palette`: `records` byte strings and `indices`.
- `sparse-soa`: `patches`, `layer_bytes`, `count_bytes`, `bounds`, `layer_indices`, `count_length`, `count_encoding`, `counts`. Counts are dense or `delta-nonzero`: start at index -1, read an unsigned LEB128 gap, advance by it, then assign the next byte. Other entries are zero.

Each reconstructed patch uses `<6fII>` bounds centre/extent, layer byte length, count byte length, then layer and count bytes. Terrain encoding is implemented in `src/terrain.rs`.

Fixed InfoPacks builds use the corresponding nonfixed object pack; fixed ZIPs are not published. ZIP reuse checks build, revision and `FORMAT_VERSION`. Bump the format version when adding animation payloads or changing layouts.

## Animations

`animations/index.json` contains `paths` (a shared string table), `bindings` (the bindings file path) and `clips`. Each clip has `object`, source `asset` index, `name`, `file`, `sample_rate`, `curve_types`, `paths` (string-table indexes) and bundle-export aliases in `source_paths`. Object IDs resolve through `metadata/sources.msgpack`.

Clip MessagePack files preserve Unity's source fields: packed/ordinary quaternion curves, position/scale/float curves, tangents, pre/post infinity values, compression flags, sample rate, optional wrap mode and events. Floats retain source float32 precision. Pointers use pack-local references. Referenced scripts or event objects are not automatically exported as visual assets; their source identity remains in the source table. Clips are shared by content and include unbound and empty clips.

`animations/bindings.msgpack` maps Animation component ID to:

- `game_object` and `transform`: owning GameObject and root transform IDs.
- `default_clip`: default clip ID or null; `clips`: source clip list, retaining order and null slots.
- `settings`: remaining Unity component fields, including enabled, wrap and automatic-playback flags.
- `targets`: `[path index, transform ID]` pairs, resolved relative to the owning GameObject. An empty path targets its root.
- `unresolved_paths`: missing path indexes; `ambiguous_paths`: `[path index, candidate transform IDs]` pairs.

Model descriptors add `animations` (binding IDs within their hierarchy) and `rigs` (rig component IDs). Graphs also list their animation and rig IDs. Resource-table Animation entries contain `{"binding": id}` instead of duplicating the binding data.

Coverage adds `animations`: clip/binding/rig counts, unbound clip IDs, missing clip references and unresolved/ambiguous target paths. Unbound means no exported Animation component references the clip; runtime code may still use it. Path issues are preserved as coverage, without renaming tracks or dropping their payloads. `missing` retains dangling source material/texture/shader references; available assets are still exported. Source references are not guessed from unrelated object IDs. CDN text placeholders are skipped and recorded as `non-bundle` entries in `scan_errors`.

These are source curves for a wiki compiler, not animations embedded in the geometry GLBs. Packed rotation times are cumulative integer deltas in hundredths of a second. Preserve their packed slopes; exact Unity interpolation and runtime layers/events are separate playback concerns. Apply the hierarchy coordinate conversion to animation values and tangents when compiling.

## Clothing Rigs

`models/rigs.msgpack` maps component ID to `script_type`, owning `game_object`, `transform` and converted source `data`. It retains ActorSkinCombiner, ActorWearIndexTable and ActorWearSkinTable components. Resource-table entries point to them with `{"rig": id}`.

Use `ActorWearSkinTable.skins` to identify garment renderers. Map each renderer's joint index through `transformIndicesM` or `transformIndicesF` into the player's `ActorSkinCombiner.actorBones`; keep that garment's weights and bind matrices. Select the matching gender's mesh through InfoPacks. An unused opposite-gender map can be shorter than the mesh's bone list.
