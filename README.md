# FFObjectPacks

FusionFall models, textures, animations and terrain for wiki previews, shipped separately from [FFInfoPacks](https://github.com/FinnHornhoover/FFInfoPacks).

| Folder | Contents |
|---|---|
| `models/` | Entity indexes, GLB geometry, hierarchies, skin weights and clothing rigs |
| `animations/` | Source curves, clip indexes and model bindings |
| `textures/` | Lossless WebP images and original-path index |
| `areas/` | Scenes, compact heightfields and detail data |
| `metadata/` | Materials, references and coverage |

Start with `manifest.json`. [FORMAT.md](FORMAT.md) describes the loader contract.

## Releases

FFInfoPacks publishes first, then triggers this repo. Run **Create Release with Artifacts** manually with no inputs to do the same. ObjectPacks discovers the latest InfoPacks release and config, matching its tag and nonfixed ZIP names. An existing matching release skips unless `FORMAT_VERSION` changes. ObjectPacks never triggers InfoPacks.

One build finishes, packages and uploads while the next downloads and scans, then waits. Only one build is prepared ahead and only one scans at a time. Downloads use five connections; conversion reserves one CPU during overlap and uses up to four threads otherwise. Shared conversions are reused within the run. Every ZIP is self-contained. Log lines use `[elapsed] [build] [prepare/scan]` or `[elapsed] [build] [finish/convert]`; waiting, uploads and total build completion are logged separately.

Fixed InfoPacks builds use the corresponding nonfixed object pack; no fixed ZIPs are published. Rerunning a release removes its old fixed ZIPs once the nonfixed packs are available.

## Development

```sh
cargo build --release --locked
cargo run --release -- extract CACHE OUTPUT --build beta-20100104 --revision 17
```

`release` runs the GitHub pipeline using `gh`, `GH_TOKEN`, `GITHUB_REPOSITORY` and `GITHUB_SHA`. Linux builds need `pkg-config`, `liblzma-dev` and `zlib1g-dev`. Image operations, BC decoding and WebP encoding use Rust crates; legacy Unity reading and the pack layout live in `src/`.
