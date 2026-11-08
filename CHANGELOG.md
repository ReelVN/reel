# Changelog

Changes to Reel are recorded here in English, starting with the 0.3.0 baseline.

## Unreleased

### Added

- Include the selected desktop render-quality tier in per-frame performance counts.
- Add a shared `preload` node property to prepare hidden mounted image and effect sources with low-priority IO and bounded spare residency.

### Fixed

- Raise desktop defaults to 1 GiB each for GPU resources and offscreen targets, 64 MiB for transient rendering data and 512 MiB for decoded CPU resources, allocating within these limits as needed.
- Prepare admitted viewport-sized desktop scratch targets and mapped upload buffers during surface setup for reuse by the first transitions and image-heavy pages.
- Account hidden prefetch residency separately from fonts and visible resources while retaining the total source-memory limit.
- Reduce virtual-list row construction outside the viewport while preserving refill hysteresis, measured-height anchoring and pinned navigation.
- Preserve desktop scratch targets across render-quality switches, reclaiming idle allocations across tiers within the shared target budget.
- Reuse materialized effects whose source is a stable ordered composition of multiple images, sharing dependency and lifetime rules across desktop and Vita.
- Record cold page preparation in the performance timeline, coalescing equal readiness states while preserving publication and presentation boundaries, callback gaps, and eviction counts.
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
- Consume submitted WGPU transition snapshots through queue ordering while retaining producer resources until GPU completion.
- Skip glyph recording for paragraphs outside the viewport's inline range, retaining font overhangs, decorations and ruby bounds.
- Omit image draws outside their paint viewport so hidden pages do not request GPU image uploads.
- Preserve rendering caches when a WGPU image upload yields at its preparation quota; reclaim them only for allocation pressure.
- Use smaller parallel conversion batches to distribute desktop image uploads across available workers.
- Allow 64 MiB per desktop image preparation to reduce completion-wake round trips when opening image-heavy pages, within the existing GPU resource budget.
- Separate CPU glyph preparation from desktop/Vita atlas uploads, resolving the font lease once per run. Desktop batches distinct missing glyphs on bounded Rayon lanes and keeps raster scratch storage owned by the renderer.
- Keep desktop screenshot targets separate from the displayed viewport, preserving target and geometry caches through thumbnail capture and reclaiming idle caches on actual budget pressure.

### Added

- Shader creation, image upload preparation and surface acquisition timing in performance captures.
- Audio device checks, format queries and native stream control timing in performance captures.
- Separate font outline, stroke expansion and alpha coverage timing in performance captures.

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
