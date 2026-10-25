# FFObjectPacks

FusionFall models, textures, animations and terrain for wiki previews, shipped separately from [FFInfoPacks](https://github.com/FinnHornhoover/FFInfoPacks). Outputs use GLB geometry, lossless WebP, source animation curves and compact terrain data.

## Contents

| Folder | Contents |
|---|---|
| `models/` | Monster, nano, NPC, player, item and prop indexes; shared geometry, skin data and clothing bone maps |
| `animations/` | Shared source clips, indexes and model bindings |
| `textures/` | Shared WebP files and original-path index |
| `areas/` | Scenes, heightfields and grass/detail data |
| `metadata/` | Materials, references and coverage |

Start with `manifest.json`. GLBs contain geometry; descriptors supply hierarchies and material bindings. See [FORMAT.md](FORMAT.md) for loader details.

## Releases

FFInfoPacks publishes first, then triggers this repo. ObjectPacks discovers the latest published InfoPacks release and uses its tag and ZIP filenames to match builds, revisions and nicknames. Download settings come from the current InfoPacks config.

Run **Create Release with Artifacts** manually with no inputs, or use `gh workflow run release.yml`. An already matching published release skips the build when its `FORMAT_VERSION` also matches. Changing `FORMAT_VERSION` rebuilds and replaces the ZIPs under the same release tag. One job installs dependencies, downloads, extracts and uploads each build on the Actions runner, then publishes. Fixed builds copy their nonfixed ZIP; they do not repeat downloads or extraction. ObjectPacks never triggers an InfoPacks build.

Builds compile a C++ decoder, use up to four conversion processes, and prefetch the next cache. Shared conversions are reused across builds within the run; every ZIP remains self-contained.

Missing cache content is listed in `metadata/coverage.json`. Assets are extracted as-is; use matching InfoPacks data for entity selection.
