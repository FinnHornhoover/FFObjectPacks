# Pack Format 3

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

Fixed InfoPacks builds use the corresponding nonfixed object pack; fixed ZIPs are not published. ZIP payload reuse checks build, source fingerprint and `FORMAT_VERSION`; revision and nickname are updated for the new release. Bump the format version when changing layouts or exported data.

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

## Appearances

`manifest.appearances` points to an index containing `hnpcs`, `palettes` and `runtime` paths under `models/appearances/`.

- `hnpcs.msgpack`: table object ID to `{name, asset, source_paths, rows, dependencies}`. Rows preserve the complete source `HNpcTableElement` array, including row 0 and unused entries. Join InfoPacks' NPC `m_iHNpcNum` to the row index when `m_iHNpc` is nonzero. Each dependency path maps to all matching pack object IDs; empty/multiple matches remain explicit. Both gender actors, skin textures and named garments/attachments are included in dependency selection. Source `iType` is female when even, male when odd; negative colour indexes retain actor defaults.
- `palettes.msgpack`: BSD object ID to `{name, asset, source_paths, skin, hair, eye}`. Serialized skin/hair colours retain source RGBA objects and float precision. `eye` is null when no serialized eye palette exists. Preserve array order: HNPC colour indexes are zero-based, while player-style indexes are generally one-based.
- `runtime.msgpack`: records with `assembly`, source `bundle`, assembly `sha256`, `method_offset`, `field` and `values`. The reader statically recognizes literal colour arrays (`skinColor`, `hairColor`, `eyeColor`), `uniqueEyes`, eye texture suffix strings (`szTextureSuffix`) and legacy GUI `eyeSwatches`. Runtime colours use RGBA float arrays. Eye swatches explicitly declare zero-based UI selection and one-based player-style indexes. They describe selector colours; rendered eyes still use face texture variants. Runtime and BSD palettes are separate: Retrobution can override the serialized colours. Unsupported recognized array initializers have `supported=false` and null values; no client code executes.

Resources for appearance objects contain `{"appearance": id}`. Their source identity resolves through `metadata/sources.msgpack`. Coverage adds table/palette counts, missing/ambiguous paths, invalid colour indexes, runtime parsing issues and actor rig/height/shape clip availability. Duplicate candidates are preserved for the wiki compiler to resolve. Missing-path records include source placeholders such as `"0"`; paths are not guessed or repaired.

## Shaders

Resource-table class 48 descriptors contain `name`, `declared_name`, `path`, `source`, `default_properties`, `static_properties` and `declarations`. Blank serialized names are recovered from the Shader declaration. `source` points to content-shared legacy ShaderLab text in `models/shaders/`; it can include compiled platform programs and is not a browser shader.

Each declaration records `scope` (`[block kind, ordinal]` pairs), `command`, tokenized `arguments` and source `line`. Tags and blend/cull/depth/alpha/colour state remain scoped to their original category/subshader/pass; apply inheritance and platform selection in the compiler. Quoted program strings and comments do not become render-state directives. Shader defaults retain their texture references, which participate in dependency selection. Coverage reports unnamed shaders and missing source text.

## Clothing Rigs

`models/rigs.msgpack` maps component ID to `script_type`, owning `game_object`, `transform` and converted source `data`. It retains ActorSkinCombiner, ActorWearIndexTable and ActorWearSkinTable components. Resource-table entries point to them with `{"rig": id}`.

Use `ActorWearSkinTable.skins` to identify garment renderers. Map each renderer's joint index through `transformIndicesM` or `transformIndicesF` into the player's `ActorSkinCombiner.actorBones`; keep that garment's weights and bind matrices. Select the matching gender's mesh through InfoPacks. An unused opposite-gender map can be shorter than the mesh's bone list.

## Source Identity

Release assets include `sources.json`: `format_version` and a `builds` map. Each build records `archive`, `files` (filename → lowercase SHA-256 of downloaded bytes) and `fingerprint` (SHA-256 of compact JSON for that sorted file map). Filenames, additions and removals affect identity; URLs and InfoPacks revisions do not. Release-built ZIP manifests add `source_fingerprint`; local extractions may omit it.
