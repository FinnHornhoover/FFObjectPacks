# Pack Format 5

Read `manifest.json` for file paths and coordinate conventions. Paths are relative to the pack root. Metadata uses MessagePack; navigation indexes use JSON.

## References and Models

- References: `{"r": object_id}` or `{"r": null}`. IDs are local to the pack.
- `metadata/resources.msgpack` and `materials.msgpack`: ID → `[Unity class ID, source asset index, descriptor]`.
- `metadata/sources.msgpack`: original asset names and Unity IDs, indexed by pack-local ID.
- Model descriptors: source path, object ID, hierarchy `graph` and `root_node`.
- Texture index: original paths → shared WebP files, including clothing variants.

Geometry GLBs preserve vertex order and use shared external `models/buffers/*.bin` files. UV V is flipped for glTF; normals/tangents are normalized. Nonfinite source UV components become zero and are counted by `invalid_uv_values`; prefab warnings identify affected renderers. Empty source meshes retain descriptors. Renderer materials bind `slot_0`, `slot_1`, etc. Original weights and bind poses remain in `models/skins/`, source curves in `animations/clips/`.

## Assembled Prefabs

`manifest.prefabs` points to `models/prefabs/index.json`: object ID → `{file, clips, renderers, warnings}`. Model catalog descriptors also include this as `prefab`. Source aliases/candidates remain separate; no archive precedence or entity appearance is guessed.

- `file`: prefab GLB, with hierarchy, source materials and skins. External geometry buffers and WebP images are shared within the ZIP. Missing skins fall back to unskinned geometry with a warning; unavailable model/clip exports are recorded in coverage.
- `clips`: ordered `{name, object, binding, file, duration}` records. Each clip GLB has the model's node order/topology but omits rest-pose TRS, meshes/skins/images, and one sampled transform animation or `extras.material_clips`. Load clips on demand and bind by node index, never node name. Duplicate names are retained by source/binding identity.
- `renderers`: source component ID → output mesh-node indexes. Mesh-node extras also contain `renderer`, `source_node` and `skinned`. Skinned meshes live at scene roots; material tracks explicitly identify their renderer nodes.
- Actor-node `extras.actor_bones` maps the actor bone table to node indexes. Garment skins retain `extras.actor_bones.m/f` maps into those actor tables, including opposite-gender length differences and negative entries.
- Saved material properties inherit missing shader defaults. Materials contain portable PBR defaults and `extras.shader`, `shader_object`, `object`, `colors`, `floats`, `textures`. Texture entries retain source identity, scale/offset and glTF texture indexes; `_MainTex` also binds base colour. WebP uses required `EXT_texture_webp`. These extras support ff-three material translation; PBR alone does not reproduce the client.
- Prefab source activity flags remain metadata: cached prefabs are commonly stored inactive and are instantiated when requested. Do not hide the preview merely because a source node is inactive.
- Root extras contain `clips` (name → relative clip URL), `renderers`, `object`, `kind` and warnings. Animation-node extras retain the clip list/default, wrap mode, enabled/automatic settings and binding ID. File references replace embedded animation indexes.

GlTF samplers use LINEAR interpolation over source Hermite samples, source key times and exact endpoints. Infinite slopes retain step boundaries using adjacent float32 times. Quaternion samples are normalized with hemisphere continuity. Redundant samples are reduced within `1e-5` per scalar/vector component or `1e-4` radians for rotation, measured against the full sample grid. Raw source curves remain unchanged. Constant tracks use one key; **duration stays in `animation.extras.duration` and document `extras.clip.duration`**, so readers must not infer source duration solely from key times. Negative source keys influence evaluation at t=0 but are not emitted as negative glTF times.

Material tracks remain in `extras.float_curves`, keyed by `path:attribute`: `{input, output, node, source_node, renderers, renderer_nodes, class_id, attribute}`. Accessors contain sampled scalar values; nonfinite samples use inline `times`/`values` with nulls. Material-only clips use document `extras.material_clips`. The source payload retains events and infinity settings; runtime layering/additive policies remain a consumer concern.

Numeric texture/accessor indexes in extras are live references. Optimizers must preserve and remap them explicitly. Ordinary pruning does not understand them. The wiki still chooses table-driven texture overrides, item slots, player/HNPC composition, palettes and entity scale.

## Hierarchies

`metadata/graphs.json` lists MessagePack graphs containing `asset`, `names`, `nodes`, `renderers`, `settings`, `external_parents`, `animations` and `rigs`.

Each 60-byte node uses little-endian `<IiII10fI`: transform ID, local parent index (-1 for root), GameObject ID (0xffffffff for none), name index, position xyz, quaternion xyzw, scale xyz, flags. Flags store active in bit 0 and layer above it.

Renderers use `[component ID, class ID, transform ID, mesh ID, attributes]`. External parents use `[transform ID, parent transform ID]`.

`metadata/node-map.bin` starts with `NMAP` + uint32 count, followed by `<IHI>` rows: object ID, graph index, node index. Transform and GameObject IDs both map to nodes.

## Coordinates

Hierarchy, terrain and bind data retain Unity coordinates. GLB geometry reflects X and reverses winding; tangent X changes sign. Reflection and the V flip cancel the tangent-W sign changes. When assembling scenes, reflect position X and quaternion Y/Z, and conjugate bind matrices by `diag(-1,1,1,1)`.

GLB UVs use `(u, 1-v)`; WebP images are vertically flipped from Unity storage. Use `flipY=false`. Client-faithful shaders use raw texture values without output colour conversion; a portable PBR fallback uses conventional colour management. Source material scale/offset values remain in Unity UV space.

## Terrain

Heights: four-byte magic + `<III>` width, height, signed flag + source-order 16-bit samples. `HMP1` is raw; `HMD1` requires cumulative sums per row modulo 65536; `HMX1` requires cumulative XOR per row. Reinterpret as int16 when signed. Scale and layer settings remain in the terrain descriptor.

Heights retain Unity's X-major order: `samples[x * height + z]`. Official-client world height is `sample / 32766 * m_Scale.y`; horizontal cell spacing is `m_Scale.x/z`. `m_Shifts` retains horizontal one-cell vertex shifts; the adjacent-cell diagonal rule must also be applied when meshing.

`manifest.world` points to `areas/world.msgpack`: object ID → `{type, asset, data}` for field-identified DongColorSetup, MapAttributeTable, EpPlatformTrigger, Waypoint and EffectEmitterController scripts, plus lights and RenderSettings. Format 5 also retains ParticleEmitterController and ParticleTrailController scripts and native ParticleRenderer (26), ParticleAnimator (12), EllipsoidParticleEmitter (15) and MeshParticleEmitter (87) records, plus MeshRenderer records owned by a trail controller (`TrailRenderer`), with their original fields and pack-local references. Group components by `data.m_GameObject.r`; EffectEmitterController particle entries reference that GameObject through `particlePrefab.r`. Renderer material references resolve through the materials metadata. Timing keys, forces, colours, billboard modes, size/rotation curves and atlas settings remain source data for the consumer. References use pack-local IDs. Graph settings retain selected script records too; effect-prefab dependencies participate in selection. World geometry, terrain blending, platform simulation, grass and area rendering are consumer work.

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

These source curves are retained alongside sampled glTF clips. Geometry-only GLBs do not embed them. Packed rotation times are cumulative integer deltas in hundredths of a second. Preserve their packed slopes; exact Unity interpolation and runtime layers/events are separate playback concerns. Apply the hierarchy coordinate conversion to animation values and tangents when compiling.

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
