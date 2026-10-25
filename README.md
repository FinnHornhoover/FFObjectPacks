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

FFInfoPacks publishes first, then triggers this repo. Run **Create Release with Artifacts** manually with no inputs to do the same. ObjectPacks discovers the latest InfoPacks release and config; tags, revisions and ZIP names match. An existing matching release skips unless `FORMAT_VERSION` changes. ObjectPacks never triggers InfoPacks.

One Rust binary downloads, extracts, packages and uploads builds sequentially. Downloads use five connections; asset conversion uses up to four threads. Shared conversions are reused within the run. Fixed builds copy the nonfixed ZIP. Every ZIP is self-contained. Progress is logged every five seconds during work.

## Development

```sh
cargo build --release --locked
cargo run --release -- extract CACHE OUTPUT --build beta-20100104 --revision 17
```

`release` runs the GitHub pipeline using `gh`, `GH_TOKEN`, `GITHUB_REPOSITORY` and `GITHUB_SHA`. Linux builds need `pkg-config`, `liblzma-dev` and `zlib1g-dev`. Image operations, BC decoding and WebP encoding use Rust crates; legacy Unity reading and the pack layout live in `src/`.
