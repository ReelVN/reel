# Changelog

Changes to Reel are recorded here in English, starting with the 0.3.0 baseline.

## Unreleased

- Predict picture paths selected from read-only module catalogs during story waits.
- Prepare configured fonts together, and start fixed-size scene image requests without waiting for appearance resources.
- Overlap inflation and row reconstruction for large PNGs on desktop when a spare worker and workspace are available.
- Add a raster export rule that stores exact decoded RGBA8 pixels in the resource archive. The player can load those pixels without running an image codec.
- Reuse live textures across saved-game restoration by matching the original asset version.
- Scan leading opaque image rows in groups and record their coverage together.
- Keep the same atlas page across consecutive glyphs and retain glyph leases only for recorded geometry.
- Resolve the host once when the Aside VM reads a host property.

## 0.4.0-dev.3 — 2026-11-20

- Skip pixel scans when an indexed texture's palette is fully opaque, and test BC3 alpha selectors together when finding image coverage.
- Quantize each visible texture pixel once and copy its edge values into storage padding.
- Reuse font and glyph lookup state across cached text runs in both renderers.
- Resolve the source of an observed Aside property once per read.
- Choose native P8 or RGB888 for single-level Vita textures when they preserve the intended appearance and reduce resident bytes. Keep RGBA8 available for translucent artwork and mip chains.
- Select the release runtime by export target. Vita defaults to the shared launcher data bundle, and cross-OS desktop exports require a runtime directory or data-only output.
- Keep GXM texture layouts separate from desktop conversion cache entries and store MP4 resources as contiguous archive ranges for direct seeking.
- Refine difficult BC3 alpha blocks during export. Use `fallback = "rgba8"` to keep scaled, upload-ready pixels for textures, tiles and atlas pages that exceed the BC quality limit. Use `convert = "copy"` to ship original files.
- Remove BC-Crunch decoding. Re-export texture assets packed by pre-release builds that used it.
- Align only row width for single-level Vita RGBA8 textures unless power-of-two padding is requested.
- Prepare known upcoming images during finite animation and delay waits.
- Admit simple lossy WebP images directly to the bounded decode queue.
- Batch ordinary scalar defaults in the VM and construct stage property tables in one pass.
- Skip transparent outer rows for new desktop textures that already need split uploads.
- Add the `qoi` desktop resource conversion for faster lossless decoding of selected 8-bit images.
- Start preparing visible fixed-size images once headers supply the dimensions needed by layout.
- Use remaining space in mapped desktop upload buffers for raster and prepared texture rows before allocating the rest.
- Skip transparent outer rows when uploading a mostly empty image to a new desktop texture.
- Keep native argument preparation on the same VM frame until the work budget or deadline requires a yield.
- Claim the desktop frame submission slot before preparing glyphs and draw resources.
- Index long runs of short literal string tests in verified Aside bytecode.
- Account bounded PNG decoder workspace separately from its resident pixel buffer.
- Reuse mapped upload buffer space across raster row ranges within one submission and report staging allocations separately.
- Decode bounded noninterlaced RGB8 PNGs directly into the resident RGBA buffer one row at a time.
- Skip model and property observer bookkeeping for ordinary VM table writes.
- Use a depth counter for ordinary clips during glyph preparation and allocate work stacks only for scopes that need them.
- Count archive output positions during export and write each compressed block directory in one batch.
- Reuse desktop geometry buffers across small changes in vertex and index counts.
- Index recorded geometry by source and command when draw order changes.
- Borrow ordinary stage constructor arguments until a property binding changes a value.
- Release encoded input as soon as final image decoding ends.
- Reuse font scaling setup across each batch of uncached glyphs.
- Check detached stage components for collection when the VM invalidates unreachable handles.
- Admit up to 8-bit PNGs with bounded ancillary data, including palettes and transparent color keys, directly to image decoding.
- Reuse the verified bytecode size at encoding time and compress debug source once when writing it.
- Share target branch selection among translation, effect, and mask compilation.
- Place raster uploads from the same desktop batch in available spans of the same staging buffer.
- Keep archive metadata available to the player while its shared IO worker is reading payloads.
- Compile a visual preparation plan into ASB so dialogue and transitions can prepare known images without walking bytecode during playback.
- Expose measured scroll limits as `max_scroll_x` and `max_scroll_y` for custom navigation and scrollbar visibility.
- Reuse each admitted literal lookup when executing the instruction or assigning a default argument.
- Use the VM's direct string-literal check for short patterns in large string matches.
- Return desktop transition textures to scratch reuse after all prepared and submitted readers have released them.
- Use queued video timestamps to schedule media updates closer to when pictures are due.
- Skip desktop upload color conversion outside decoded images' nonzero-alpha bounds.
- Reuse spare desktop viewport textures for large cropped groups during multisampled rendering.
- Raise existing image and shared atlas requests to visible priority when a scene needs them, including reads that are already in progress.
- Reuse recently uploaded images across story restarts and save loads in the same game without keeping a second copy of the pixels on the CPU.
- Start loading packed resources from their mounted lengths and versions without waiting for a separate metadata request when the index is immediately available.
- Record desktop event loop waits separately from frame preparation and GPU submission.
- Reduce desktop offscreen allocations during animated bounds changes by sharing wider multisampled scratch capacity classes. Padding stays within the viewport on axes where the original target fits.
- Reuse desktop draw bindings for stable images, glyph atlas pages and solid fills across frames.
- Size the desktop player's initial texture upload buffers from the window's pixel dimensions. Larger image uploads continue to allocate on demand.
- Reuse desktop image staging buffers for video uploads once the GPU has finished reading them.
- Remove the node's `preload` property. The player uses spare loading capacity to prepare a retained modal page's visible layers while the page is hidden.
- During a dialogue wait, the player can prepare the next image or a literal group of image layers, including descriptors forwarded through helpers or selected by matching a known expression name. Scrolling, controls and dialogue share the same spare image budget. Masked transitions also start preparing their mask while the old scene is still on screen.
- `layered_image` accepts an array of image descriptors. Bind the whole composition, one layer, or individual properties with ordinary bindings. Existing scripts passing a callback can wrap it in `derive`.
- `image_skin` and `appearance` share pointer and navigation artwork through `highlight`. A skin state can use a crop tuple from its base image. The player uses spare loading capacity to prepare a visible skin's alternate files.
- Loose-file playback discovers the available platform resource directories once per session. Games that ship only common assets open each file directly. Performance reports separate resource lookup from payload reads and show how often the player opens a loose file or queries its metadata.
- Playing a project with asset conversion rules uses its in-memory resource index. It no longer rewrites the distribution index on launch or queries unchanged files twice for their size.

## 0.3.15 — 2026-11-18

### Changed

- On Windows, starting a new story or loading a save prepares its replacement audio stream off the game thread. The player releases the previous mixer before handing the endpoint to the new session.
- Desktop players decode up to three independent RGBA8 PNG layers in parallel within the game's existing memory budget. PNGs with metadata retain the full decoder allowance.
- Loading an upcoming image in a hidden subtree preserves the visible layout. Moving a node while its image is loading reuses its existing requests.
- Reuse the verified bytecode digest when writing and loading an ASB.
- Reuse existing translation archives before compressing their contents during a build.
- Desktop saves and builds publish the synced staging file directly, removing an extra file rename per write.
- Aside reuses the iterator and its source when advancing through arrays, tuples and integer ranges.
- UI frame preparation skips payload copying for fixed-size draw commands. Occlusion planning allocates clip and isolation stacks as the drawing enters those scopes.
- Audio decoding reuses existing buffer reservations across packets. Resizing a reservation to its current size no longer locks the resource budget.
- The Aside syntax cache reuses subtree hashes as it grows. The editor reuses parsed modules during linking, and the compiler indexes native declarations once per build.
- Single-node stage updates avoid a temporary heap buffer. Image requests reuse the stage index when deciding whether fixed-size images need viewport layout.
- Audio tracks mix and clear only the scratch samples consumed by each output block.

### Fixed

- Report diagnostics and navigate to definitions in the project root module at `src/mod.aside`.
- Concurrent requests for the same font or effect shader can share a single resident copy.
- Use the current viewport to decide whether a failed image tile blocks the frame.
- Load saves after a story edit removes a local value that was live at the saved line. Loading reports an incompatible point if the updated story replaces the saved dialogue with a different kind of wait.
- Release temporary VM roots when checkpoint restoration rejects malformed data.

## 0.3.14 — 2026-11-18

### Changed

- Assemble the final ASB directly from the compiled program and prepared resource manifest during builds and exports.

### Fixed

- Wake desktop playback for gamepad input and background work without redrawing an unchanged scene.
- Exit desktop playback as soon as session cleanup finishes, even with a pending wake deadline.
- Complete public exports after a module separator inside a grouped Aside import.
- Identify the native function and missing parameter when a compiled game uses an older host signature.

## 0.3.13 — 2026-11-17

### Added

- Capture frame timings and source diagnostics while reproducing an issue in a desktop development session.
- Verify a target export and its completed playback against required scenes, submission intervals and memory limits.

### Changed

- Reuse one desktop brush texture for solid and gradient drawing across frames.

### Fixed

- Reject unsupported desktop MP4 audio tracks during export instead of discovering them at playback.
- Keep native widget animations on the presentation cadence while script clocks use their own deadlines.
- Retry save restoration after collecting unused VM objects and report capacity errors without replacing the running session.
- Return the shared desktop brush texture to the render budget when GPU work releases it and another resource needs space.
- Check performance policies before launching playback and associate frame measurements with their original request.
- Flush queued Windows audio at session changes while reusing the negotiated device format.
- Resolve imports from unsaved editor documents when Windows path spelling differs from the filesystem.

## 0.3.12 — 2026-11-17

### Changed

- Mark leaf VM objects without queueing them, and scan adjacent references during one heap lookup.
- Reuse wrapped text measurements at the same width even when the text overflows.
- Convert identical source pixels once per block when uploading transparent desktop textures.
- Report text shaping, wrapping and paint-bound indexing separately in performance captures.
- Scan image transparency and opacity four pixels at a time during resource registration.
- Reuse scaled outline bounds when rasterizing filled glyphs.
- Keep native widget animation on a fixed sampling cadence through late redraws.

## 0.3.11 — 2026-11-17

### Changed

- Scan VM frame roots in contiguous blocks, looking up each register window once per GC slice.
- Release unreachable VM value blocks directly during garbage collection.
- Allocate audio segment queues on demand. Sounds played without queued segments use no queue storage.
- Reuse the initial desktop audio device probe on first configuration, avoiding a second format enumeration during restore.
- Reuse the desktop audio device and native stream when starting a story or loading a save.
- Keep isolated effect results reusable across transitions and masks on other subtrees.
- Reuse the WGPU shader module and layouts across effect variants that differ only in vertex input or blending.
- Generate smaller desktop shaders by sharing work across uniform branches and keeping large weighted sampling sums in ordered loops.
- Upload fully opaque desktop sRGB artwork directly as 8-bit textures. Images with transparency retain linear premultiplied storage.
- Copy aligned opaque images in one block and avoid clearing desktop upload buffers a second time.
- Skip upload admission for resident desktop images and publish only completed new uploads to the cache.
- Use 64 MiB Vulkan allocation blocks to reduce large allocation pauses during desktop image creation.
- Reuse recently uploaded images for prefetch instead of reading and decoding the same source again.
- Reserve desktop RGBA8 PNG output directly from its header and decode it in one background request.
- Keep pending preloads when returning foreground images can be reused from the GPU.
- Prefetch fixed-size images approaching the viewport so animated entrances can reuse decoded art.
- Compute glyph ink bounds only for render groups that need them.
- Reclaim idle image storage by demand, starting with retired storage and then the least recently used textures.
- Reuse image dependency indices for mounted virtual list rows.
- Reuse the resolved archive entry across read blocks on immutable mounts.
- Cull draws hidden by a fully opaque, in-bounds image crop.

### Fixed

- Continue in-flight image preloads when the image becomes visible, including shared atlas pages.

## 0.3.10 — 2026-11-11

### Changed

- Set the Vita player version to `00.40` and change its Title ID to `REELVITAP`. Vita installs it as a separate LiveArea bubble and reads the existing library and game saves from `ux0:/data/reel/`.

## 0.3.9 — 2026-11-10

### Added

- Add vertical row stops to scroll viewports and their native scrollbars.

### Changed

- Refresh the Vita launcher with a two-column library, clearer selection and contextual controller hints. Circle confirms and Cross goes back.
- Keep desktop client sizes in physical pixels at every display scale, independently of the logical canvas.
- Export distributable projects to `dist/<target>/<profile>/`. Move any existing `build/export/` packages to `dist/` before running `reel clean`.
- Reuse desktop shader modules across mesh, sprite instance, coverage and mask blend pipelines.
- Build independent desktop render pipelines in parallel during startup and when a mask is first used.

### Fixed

- Reject desktop masks that exceed the device's texture binding limit before creating their pipelines.

## 0.3.8 — 2026-11-10

### Added

- Allow uncapped animation sampling while preserving event-driven redraw for idle scenes.
- Share desktop resource overlays and conversion rules across Windows, Linux and macOS, with exact target overrides.
- Include whole-segment submission counts and memory peaks in playback summaries.

### Changed

- Use the latest transcript line as the default save-slot title.

### Fixed

- Reuse prepared atlas pages without opening source image decoders.
- Wake the desktop event loop on background resource, save and media completions independently of window redraw.
- Share WGSL shader modules across desktop paint and composition pipeline variants.
- Warm the preferred desktop rendering tier at surface setup and prepare fallback pipelines on demand.
- Run ready save restoration steps within one host time slice.
- Preserve virtual-list geometry revisions when the committed row window is unchanged.

## 0.3.7 — 2026-11-10

### Added

- Expose gamepad button display labels through `stage.input().buttons`.
- Add budgeted literal string splitting and replacement with Unicode-aware empty patterns.
- Check MP4 video profiles during desktop resource preparation and point incompatible files to the video conversion rule.

### Fixed

- Generate save continuations after completed `for` loops.
- Explain how to annotate a checkpoint method receiver when its nominal type cannot be proved.
- Report task launches in node blocks, unsafe host calls in derived expressions, and attempts to await known non-awaitable transition results at their source locations.

## 0.3.6 — 2026-11-10

### Added

- Generate save continuation positions for verified waits and calls inside a story flow.
- Let games enable recent dialogue jumps from their history menu.
- Add `std::math::min` and `max` for scripts and shaders. Scripts preserve the selected operand's numeric type.
- Load static WebP artwork on desktop and use it as input for texture, tile and atlas preparation.
- Convert 8-bit artwork to lossless WebP with a resource target rule.
- Let texture rules preserve the source image when BC encoding exceeds the display-error threshold.

### Fixed

- Share transcript pages with a save capture to reduce its main-thread work.
- Keep a frame capture pending while the renderer waits for submission capacity.

## 0.3.5 — 2026-11-09

### Added

- Show module declarations and class members in the editor outline, and offer a quick fix for missing semicolons.
- Add `progress_bar` for bindable progress displays with custom colors and overlay children.
- Find references to Aside bindings and exports across the active and entry import graphs.
- Import project modules from `src/` with paths such as `use ui::dialogue;`.
- Handle tab selection before committing it with `tab_bar(on_select: ...)`.
- Add `tooltip` for passive content anchored to a control's hover or visible focus.
- Splice arrays and tuples into component children with `...items;`.
- Use shorthand fields such as `{width, height}` in table literals.
- Add `input(on_back: ...)` for default back controls and `Scope.wait()` for pages ended by callbacks.
- Add `PageStackNode.select` to commit the latest page selection after construction.
- Destructure object fields in `let` and `for` bindings.
- Complete valid image fit, sampling and scroll overscroll values in the editor.
- Read plain or bound values through `value_of` in component wrappers.
- Add `first_where` and `any` for short-circuit collection predicates.
- Complete item and row members inside inline keyed list callbacks using the item source.
- Compose sliders with custom track and thumb nodes that share the control's input geometry.
- Select image assets and atlas regions from a control's interaction state with `image_skin`.
- Reveal mounted nodes in their scroll viewport using presented bounds, optional margins and animation.
- Initialize missing table entries lazily with `get_or_insert_with`.
- Annotate named return parts with structural table types such as `{node: Node, caption: TextNode}`.
- Constrain menu results and other values with string literal types, and complete finite choices in the editor.

### Fixed

- Resolve prelude types such as `Node` in signatures imported from project modules.
- Preserve known types through null comparisons and `??` fallback expressions.
- Preserve guard refinements after an early exit from an `if` branch.
- Pass final static record spreads directly as named arguments.

## 0.3.3 — 2026-11-09

### Changed

- Bind native calls from the same host registration used to validate the package, avoiding a second registry search at startup.
- Bind and validate external text once for both code execution and continuation recovery.
- Transfer transition data directly to the frame compiler and reuse localization fallback templates without allocating a cache key on hits.
- Share inline shader operand lists across generation, branch analysis, and footprint analysis.
- Build performance summaries from the current capture without rereading the generated reports.
- Reuse generation paths during save cleanup and give staging directories a single owner.
- Reuse compression buffers within each archive and report export sizes from the bytes written.
- Skip the extra device-buffer clear when audio conversion fills the entire callback.
- Use a bounded LRU for editor disk sources and index native declarations during host binding.

### Removed

- Remove the environment switches for earlier Aside execution experiments and retire three ad hoc VM benchmark tests.
- Use `host_resume_value` for both callback and wait results, replacing the old `host_callback_value` alias.
- Remove support for deriving performance counters from older traces missing session diagnostics, interval boundaries, or VM phase buckets.

## 0.3.2 — 2026-11-09

### Added

- Add `std::math::clamp` for inclusive numeric ranges with exact mixed integer and float comparisons.

### Changed

- Fill growing Aside arrays directly in bounded storage blocks and validate each repeated value once per block before publishing the new length.
- Reuse validated table positions for repeated integer-key lookups.
- Scan arrays by storage block when attaching them to reactive models, skipping scalar entries and repeated references.
- Batch garbage collection scans and clearing within storage blocks while honoring edge and work budgets.
- Reuse virtual list row measurements during scrolling and send scroll feedback when input or layout changes the requested state.
- Keep periodic script trace writes out of desktop performance captures unless diagnostic tracing is enabled.

## 0.3.1 — 2026-11-09

### Added

- Use one `point` declaration for recoverable interaction and ordinary flow calls, with compiler-generated continuation layouts.
- Infer a Root's State from its ordinary defaulted parameter, which new games evaluate and loading supplies directly.
- Load player saves through current Root code and stable story keys, preserving typed state, dialogue reveal, choices and history across code updates.
- Pass current Root services through `run_flow`'s optional context, reconnecting live aliases to rebuilt views and callbacks on loading.
- Preserve shared image/stack references, direct property bindings and leaf tween, delay and clock progress in logical state graphs.
- Add composable native scrollbars with custom track/thumb content, bounded scrolling, pointer capture, keyboard/controller input and accessibility range actions.
- Retain verified stable continuation metadata through source compilation, ASB release builds, external text binding and distribution re-encoding.
- Allow stable continuation points at calls to statically resolved script methods, restoring their receiver from the current story context.
- Expose `saves.can_capture()` so menus can enable saving at supported story positions.
- Add budgeted typed-state graph and business-continuation capture/restoration APIs for embedding hosts, preserving current class behavior, shared references and defaulted new fields.
- Add native `page_stack` and `tab_bar` compositions with retained lazy pages, shared construction tasks, customizable tab content, and keyboard/controller navigation.
- Add opt-in touch overscroll with native rebound animation while preserving bounded logical scroll positions.
- Include the selected desktop render-quality tier in per-frame performance counts.
- Add a shared `preload` node property to prepare hidden mounted image and effect sources with low-priority IO and bounded spare residency.
- Record shader creation, image upload preparation and surface acquisition timing in performance captures.
- Record audio device checks, format queries and native stream control timing in performance captures.
- Record font outline, stroke expansion and alpha coverage timing separately in performance captures.

### Fixed

- Share continuation analysis across the points in each flow to reduce build time for long chapters.
- Share identical continuation layouts within each flow to reduce ASB size and retained metadata.
- Encode checkpoint sites as compact records and reference current callees by function ID. Rebuild ASBs containing earlier development checkpoint tables.
- Reject window changes during isolated save restoration so a Root can continue preparing its view.
- Keep active tweens running when a view attaches to their source through a batched property binding.
- Continue scrollbar drags from the corrected offset when virtual rows refine the measured range.
- Infer direct lambda parameters and results from expected callable types in calls, typed bindings, assignments and returns, including native page and tab factories.
- Preserve authored scroll animations through virtual-list anchor feedback and layout clamping while retaining explicit input takeover.
- Keep recent frame samples separate from historical slow frames across performance-label changes.
- Preserve identity-key ordering when restoring VM snapshots so retained lists, page caches and in-progress keyed sorts continue correctly.
- Preserve virtual-list viewports through nested containers, clipping and transforms, with indexed scroll ownership and consistent positioning and image-readiness checks.
- Keep large reusable desktop image-upload buffers when smaller completed uploads fill the staging cache.
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
