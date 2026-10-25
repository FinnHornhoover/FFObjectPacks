# Pack Format 1

Read `manifest.json` for file paths and coordinate conventions. Paths are relative to the pack root. Metadata uses MessagePack; navigation indexes use JSON.

## References and Models

- References: `{"r": object_id}` or `{"r": null}`. IDs are local to the pack.
- `metadata/resources.msgpack` and `materials.msgpack`: ID → `[Unity class ID, source asset index, descriptor]`.
- `metadata/sources.msgpack`: original asset names and Unity IDs, indexed by pack-local ID.
- Model descriptors: source path, object ID, hierarchy `graph` and `root_node`.
- Texture index: original paths → shared WebP files, including clothing variants.

GLBs preserve vertex order, UV sets, colours and tangents. Renderer materials bind `slot_0`, `slot_1`, etc.; source shader properties live in the material table. Bind poses and weights are in `models/skins/`. Animation clips and glTF skins are not exported yet.

## Hierarchies

`metadata/graphs.json` lists MessagePack graphs containing `asset`, `names`, `nodes`, `renderers`, `settings` and `external_parents`.

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

Each reconstructed patch uses `<6fII>` bounds centre/extent, layer byte length, count byte length, then layer and count bytes. Reference decoders are in `scripts/ffobjects/terrain.py`.

ZIP reuse checks build, revision and `FORMAT_VERSION`. Bump the format version when adding animation payloads or changing layouts.
