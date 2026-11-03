# Changelog

Changes to Reel are recorded here in English, starting with the 0.3.0 baseline.

## Unreleased

## 0.3.0 — 2026-11-03

This release establishes the current development baseline for the engine, Aside tooling and players.

### Added

- Performance capture through the ordinary player, with `reel-dev perf` commands to prepare runs, collect frame and VM timings, and summarize results on desktop and Vita.

### Changed

- Reduced VM call-frame movement and repeated method lookup; added compact execution paths for derived values and direct native calls.
- Reused retained scene data and intermediate effect targets, and cropped effect work to the required region.
- Replaced recursive UI tree lookup with flat `tree_arena` storage.
- Adopted maintained libraries for LRU caching, atlas packing, source line indexing, command parsing, directory traversal and desktop temporary files.
- Stored launcher preferences in `launcher.json`. Existing TSV preferences must be set again in the launcher.
- Updated the Vita backend dependency with target views, write regions and explicit tracking of GPU users of retained resources.
- Unified workspace package versions at 0.3.0 and Vita package metadata at 00.03.

### Fixed

- Large session ownership transfers that could overflow the Windows stack during save capture and restoration.
- Dialogue reveal and automatic-advance timing when reading is suspended and resumed.
- Resource descriptor accounting for index storage and resource reservations.
- Uncovered stage regions now use a black background.
