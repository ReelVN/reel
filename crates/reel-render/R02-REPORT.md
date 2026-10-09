# R02 — real renderer and native diagnostic

The optional wgpu adapter now consumes actual R01 RenderFrame records and submits
real command buffers. The winit assembly displays the same production output and
routes pointer input through PresentationBridge. This is window/render integration,
not a playable Aside session. No VM execution, independent VM test bypass, main
update, PR merge, ready transition or Actions enablement was performed by this task.

Started on an independent `feat/r02-wgpu-player` branch from PR #27
`411cc7531431f35fbd43a614476815e22cd58f21`, then synchronized exact dependency
`9578cb3991d62a5dffb6a8580a1a6d1a850230cc`. The R01 branch was not modified.
After the integration owner explicitly authorized main as the new dependency,
merged exact main `9c2f768f53461770985f8872d3a833542e83aed5` into this branch.
Only Cargo.lock conflicted; resolution preserved pinned renderer dependencies
and added main's workspace packages. No VM target was executed. The draft PR now
targets main; neither main nor the original R01 branch was modified by this task.
Before delivery main advanced to `6b17f7966ded2376a3e45e278742ebee7c3e0321`
(PR #26 lowering). The final normal merge resolved only Cargo.lock; scoped
render/core/stage/player check and strict Clippy passed again, with no renderer
source change or VM execution. See [main refresh](evidence/r02/main-refresh.log).


## Implementation boundaries

- `gpu/device.rs`: actual adapter/format capabilities, explicit quality selection,
  stable generation, device loss and captured driver errors.
- `gpu/renderer.rs`: opaque preparation, admission slots, owned budget tickets,
  actual queue submission and separately observed completion. Foreign renderer
  identity is checked independently of generation. Shutdown waits before release.
- `gpu/replay.rs`, `draw.rs`, `mesh.rs`, `glyph.rs`, `resources.rs`: direct command
  consumption, Lyon geometry, Swash outlines and linear-premultiplied image upload.
  No second Scene or text shaping is introduced. Cache identity is the complete
  ResourceHandle within one generation-bound preparation and its versioned lease.
- `gpu/targets.rs`, `pipeline.rs`, WGSL: separately charged MSAA/resolve targets,
  whole-group opacity, geometric clip coverage, alpha/luminance masks and frozen
  crossfade. Offscreen submission cannot report PresentAccepted.
- `reel-player/window.rs`, `shell.rs`, `diagnostic.rs`: native surface/window
  assembly, resize/DPI/zero size, focus/capture cleanup, suspend/resume, bounded
  completion wake-up, device recreation and clean exit. Only surface present is
  followed by resolving the matching frame's acceptance notice.

Full device ownership, limits, supported/rejected records and the distinction
from R01's simulated RenderBackend API are in [R02-INTERFACE](R02-INTERFACE.md).
The earlier recording/null implementations remain unchanged.

## Executed evidence

Environment: Linux x86_64, Rust 1.96.0. The container initially had neither Rust
nor Vulkan ICD/display server. Rust was installed under `/workspace`; Debian
Fontconfig/Freetype development metadata and Mesa/Xvfb/xdotool packages were
extracted under `/workspace/reel-sysroot`. No system directory changes or elevated
execution were needed. Vulkan loaded **Mesa llvmpipe 25.0.7, LLVM 19.1.7, CPU
adapter**, not a physical GPU. Xvfb provided an actual X11 presentation surface.

Seven explicit real-device tests pass in debug and release:

| Test | Evidence |
| --- | --- |
| Linear premultiplied frozen-frame crossfade | Pixel readback matches red alpha 128/255 mixed with opaque blue; RGBA16Float/4x selected |
| Whole-group opacity and nested masks/clips | Overlap is blue alpha 0.5, not per-child transparency; nested alpha then red luminance gives 0.2126 × 128/255 × 0.5; outside clip is zero |
| Transformed path clip | Lyon triangle coverage under nonuniform scale accepts interior and rejects points inside only its bounding box |
| Image/text/retirement | sRGB 128 with alpha 128 converts to analytical linear value; real Parley gives 14 original glyphs, Swash readback covers 593 pixels; retired image source survives until completion |
| Admission/completion | Two preparations fill admission; a third is refused; dropping one cancels it; frame/target tickets survive queue execution until completion is observed |
| Quality and device loss | Configured budgets explicitly select RGBA16Float/1x, RGBA8/1x or refusal; actual Device::destroy triggers loss; new device is usable |
| Localized refusal/device identity | Unsupported blend rejects the entire frame at its exact RecordLocation with no acceptance; a foreign renderer's preparation is rejected and releases its reservations |

Raw outputs: [GPU debug](evidence/r02/gpu-debug.log),
[GPU release](evidence/r02/gpu-release.log).

Actual X11 events moved the white indicator from x=135–144 to x=275–284, then
scaled it to x=330–341. A 700×460 resize changed surface epoch. F5 destroyed the
actual device; recreation reported generation=2/epoch=3 and accepted a new frame.
F6 exercised the shared suspend/resume path and accepted epoch=4. Escape returned
exit status 0. [Window log](evidence/r02/window.log),
[pixel measurements](evidence/r02/window-pixels.json),
[before](evidence/r02/window-before.png), [drag](evidence/r02/window-drag.png),
[recovered](evidence/r02/window-recovered.png).
F6 is controlled lifecycle injection, not proof of an OS-originated suspend event.

The existing regressions pass in debug/release: 12 core, 24 U02 behavior contracts,
one zero-allocation pointer-dispatch contract, 14 R01 cases and six recording/null
render cases. Real Parley was also run explicitly in both profiles. Workspace
fmt and all-target/all-feature strict Clippy pass. Feature-off check passes and
the normal feature-off dependency tree contains neither wgpu nor winit.
[Debug](evidence/r02/debug.log), [release](evidence/r02/release.log),
[Clippy](evidence/r02/clippy.log), [feature-off](evidence/r02/feature-off.log),
[dependency tree](evidence/r02/feature-off-tree.log),
[Parley debug](evidence/r02/parley-debug.log),
[Parley release](evidence/r02/parley-release.log).

After syncing main, the scoped verification script passed again, including both
GPU profiles: [integrated validation](evidence/r02/integration.log). Diagnostic
asset versions now hash the actual bytes with pinned BLAKE3 1.8.2; the optional
font is precharged and capped at 16 MiB before reading.

Commands and environment are in [commands](evidence/r02/commands.txt).
[check-r02.sh](../reel-player/scripts/check-r02.sh) restricts future checks to the
four render/stage/core/player packages, so integrating unrelated VM packages
cannot silently expand this task's execution scope. [Dependency versions and
licenses](evidence/r02/dependencies.txt) retain ordinary pinned upstream packages;
no new source fork or native handle was added to stage/core.

## Remaining scope and target limitations

Physical GPU, Wayland, Windows, macOS, mobile OS suspend/resume, true hot-unplug
and display scanout were not verified. PresentAccepted means successful
acquisition/submission/present API acceptance, not compositor scanout or queue
completion. Independent hardware validation remains necessary.

The portable device profile deliberately refuses unsupported gradients, blend
modes, filters/blur, External producers, dash patterns, repeat/non-bilinear image
sampling and advanced/color glyph records. Their exact limitations are listed in
the interface; they remain full engine work, with no silent omission.

Persistent mesh caches, glyph atlas and adjacent-draw batching are not implemented.
Images are reused within one preparation; font rasterization is repeated. All
full-viewport offscreen allocations are charged for the submission lifetime;
large/nested scenes can explicitly exceed the default 128 MiB target budget.
Target pooling/lifetime reuse and measured performance tuning remain needed.
Driver allocation overhead and private Lyon/Swash scratch are not a proven hard
memory bound. This report makes no total-VRAM, latency or frame-rate guarantee.
The first shell is a focused mouse/keyboard diagnostic, not full platform IME,
accessibility, touch/pen or engine session integration.
