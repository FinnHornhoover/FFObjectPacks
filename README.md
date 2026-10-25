# FFObjectPacks

FusionFall models, textures and terrain for wiki previews, shipped separately from [FFInfoPacks](https://github.com/FinnHornhoover/FFInfoPacks). Outputs use plain GLB, lossless WebP and compact terrain data.

## Contents

| Folder | Contents |
|---|---|
| `models/` | Monster, nano, NPC, player, item and prop indexes; shared geometry and rigs |
| `textures/` | Shared WebP files and original-path index |
| `areas/` | Scenes, heightfields and grass/detail data |
| `metadata/` | Materials, references and coverage |

Start with `manifest.json`. GLBs contain geometry; descriptors supply hierarchies and material bindings. See [FORMAT.md](FORMAT.md) for loader details.

## Releases

FFInfoPacks publishes first, then triggers this repo. ObjectPacks discovers the latest published InfoPacks release and uses its tag and ZIP filenames to match builds, revisions and nicknames. Download settings come from the current InfoPacks config.

Run **Create Release with Artifacts** manually with no inputs, or use `gh workflow run release.yml`. An already matching published release skips the build when its `FORMAT_VERSION` also matches. Changing `FORMAT_VERSION` rebuilds and replaces the ZIPs under the same release tag. One job installs dependencies, downloads, extracts and uploads each build on the Actions runner, then publishes. ObjectPacks never triggers an InfoPacks build.

Missing cache content is listed in `metadata/coverage.json`. Assets are extracted as-is; use matching InfoPacks data for entity selection.
