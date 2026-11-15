# FFObjectPacks

FusionFall models, textures, animations, audio and terrain for wiki previews, shipped separately from [FFInfoPacks](https://github.com/FinnHornhoover/FFInfoPacks).

| Folder | Contents |
|---|---|
| `models/` | Assembled prefab GLBs, shared buffers, source hierarchies, clothing rigs, appearances and shaders |
| `animations/` | Separate glTF clips, source curves and model bindings |
| `audio/` | Shared Ogg clips, animation sound events, region and emitter metadata |
| `textures/` | Lossless WebP images and original-path index |
| `areas/` | Scenes, compact terrain, colours, platforms and effect metadata |
| `metadata/` | Materials, references and coverage |

Start with `manifest.json` and its `prefabs` index. Prefabs include source materials and skins; clips load separately. The wiki supplies InfoPacks-driven texture swaps, dressed-player/HNPC composition and entity scale. Terrain remains compact source data. [FORMAT.md](FORMAT.md) describes the loader contract.

## Releases

FFInfoPacks publishes first, then triggers this repo. Run **Create Release with Artifacts** manually with no inputs to do the same. ObjectPacks discovers the latest InfoPacks release and config, matching its tag and nonfixed ZIP names. An existing matching release skips unless `FORMAT_VERSION` changes. ObjectPacks never triggers InfoPacks.

Each build scans and extracts in a fresh worker process, releasing its heap before packaging and upload. The next build's download is prefetched; scans do not overlap. Workers use up to four conversion threads and share conversions on disk. Decoded animation clips use a 256 MiB estimated LRU cache. Downloads use five connections with five attempts and exponential backoff. Every ZIP is self-contained. Logs identify the build, operation and worker PID; worker elapsed times restart per build.

Releases include `sources.json` with per-build file hashes and fingerprints. Unchanged sources and format reuse the previous ZIP's compressed payloads, updating revision metadata. Published hashes avoid cache downloads; other sources are hashed during download. Older releases without fingerprints need one baseline extraction.

Fixed InfoPacks builds use the corresponding nonfixed object pack; no fixed ZIPs are published. Rerunning a release removes its old fixed ZIPs once the nonfixed packs are available.

## Development

```sh
cargo build --release --locked
cargo run --release -- extract CACHE OUTPUT --build beta-20100104 --revision 17
```

`release` runs the GitHub pipeline using `gh`, `GH_TOKEN`, `GITHUB_REPOSITORY` and `GITHUB_SHA`. Linux builds need `pkg-config`, `liblzma-dev` and `zlib1g-dev`. The glTF/terrain implementation follows [Kamil’s UnityPackFF fork](https://github.com/kamilprzyb2/UnityPackFF), with material metadata for [ff-three](https://github.com/kamilprzyb2/ff-three); see `NOTICE`. Image operations, BC decoding and WebP encoding use Rust crates; legacy Unity reading and the pack layout live in `src/`.
