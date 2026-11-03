# Changelog

Changes to Reel are recorded here in English, starting with the 0.3.0 baseline.

## Unreleased

### Fixed

- Wait for image and font readiness before requesting another redraw, avoiding repeated UI work while resources load.
- Reuse glyphs uploaded during retained-scene preparation in the following WGPU draw.
- Convert desktop RGBA8 uploads with exact channel/alpha lookup tables and copy pixels to mapped GPU buffers in batches.
- Divide large desktop image conversions into disjoint rows on Rayon workers, preserving pixel values and upload memory limits.
- Reuse the desktop audio device's query client when resuming an existing stream, avoiding repeated capability enumeration on UI sounds.
- Prepare WGPU coverage pipelines during startup so the first clipped page reuses an existing pipeline.
- Expand glyph stroke outlines once for both bounds and rasterization in the shared desktop/Vita text path.
- Preserve reusable intermediate targets when preparing a retained transition, excluding the frozen output from scratch reuse.
- Keep scratch render targets within their existing memory budget until allocation pressure or lifecycle cleanup reclaims them, avoiding repeated allocation after brief gaps in use.
- Use WGPU's performance allocation policy for desktop GPU buffers and textures.
- Preserve the presentation surface and accepted input geometry on repeated window-size notifications.
- Prepare the stopped desktop audio stream during output configuration so the first sound reuses it.
- Size glyph atlas entries for the physical display scale using half-octave cache tiers, including scaled-down canvases.

### Added

- Shader creation, image upload preparation and surface acquisition timing in performance captures.
- Audio device checks, format queries and native stream control timing in performance captures.

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
