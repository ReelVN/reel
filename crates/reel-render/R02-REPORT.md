# R02 renderer and native diagnostic

The optional wgpu renderer consumes R01 RenderFrame directly and submits real
command buffers. The winit player displays its production output and dispatches
input through the matching accepted PresentationBridge geometry. This diagnostic
is window/render integration, not an Aside story session. No VM was executed.

The concrete API, accounting, supported/rejected records and full remaining engine
scope are in [R02-INTERFACE](R02-INTERFACE.md). Native handles stay in player.

## Correctness follow-up

- Drawing and clip strokes validate f32 width/miter parameters before Lyon. Legal
  R01 miter values 0 and 0.5 are localized GPU refusals for all joins; 1 renders.
  No panic, clamp, false acceptance or leaked reservation occurs.
- Single-preparation quotas distinguish permanent oversize from temporary shared
  residency pressure. The player retains latest state, waits for observed queue
  completion and retries without requiring another input event. Accepted input
  geometry changes only after the retry is actually presented.
- Depth and malformed scope errors are localized permanent refusals. Admission
  Busy remains temporary. Permanent errors cannot cause repeated 8ms preparations.
- Resize, surface recreation and device recovery freshly negotiate capabilities
  and rebuild SurfacePass. An incompatible format/alpha/present-mode/usage set is
  refused. Old device preparations and surface candidates remain invalid.
- OS expose is independent of model dirtiness. Successful idle frames redraw;
  blocked latest models repaint one bounded, frozen accepted CPU frame without
  compiling the bad model or changing accepted geometry. Timer/completion polling
  alone schedules neither a clean frame nor a permanent retry.

The follow-up does not add persistent caches/atlas, scene-wide quality selection,
white-texture or pipeline reuse. Unsupported records and target limitations in the
interface remain required full-engine work.

## Reproducible checks

Run from the repository root; use Rust 1.96 and native Freetype/Fontconfig metadata:

```sh
cargo fmt --all -- --check
cargo clippy --locked -p reel-core -p reel-stage -p reel-render -p reel-player --all-targets --all-features -- -D warnings
cargo test --locked -p reel-core -p reel-stage -p reel-render -p reel-player --all-features -- --test-threads=1
cargo test --locked -p reel-core -p reel-stage -p reel-render -p reel-player --all-features --release -- --test-threads=1
cargo check --locked -p reel-core -p reel-stage -p reel-render -p reel-player --no-default-features
cargo tree --locked -p reel-core -p reel-stage -p reel-render -p reel-player --no-default-features -e normal
```

The feature-off normal dependency tree must contain neither wgpu nor winit. With
an actual adapter, real font and (for the Linux window check) X11 display:

```sh
export REEL_R01_TEST_FONT=/path/to/real-font.ttf
cargo test --locked -p reel-render --features wgpu --test gpu_pixels -- --ignored --nocapture --test-threads=1
cargo test --locked -p reel-render --features wgpu --test gpu_pixels --release -- --ignored --nocapture --test-threads=1
cargo test --locked -p reel-render --features wgpu --test gpu_correctness -- --ignored --nocapture --test-threads=1
cargo test --locked -p reel-render --features wgpu --test gpu_correctness --release -- --ignored --nocapture --test-threads=1
cargo test --locked -p reel-stage actual_parley_label_glyphs_are_copied_without_reshaping -- --ignored --nocapture
cargo test --locked -p reel-stage --release actual_parley_label_glyphs_are_copied_without_reshaping -- --ignored --nocapture
cargo test --locked -p reel-player --features window actual_window_retry_and_recovery_contract -- --ignored --nocapture --test-threads=1
cargo test --locked -p reel-player --features window --release actual_window_retry_and_recovery_contract -- --ignored --nocapture --test-threads=1
```

These commands select only core/stage/render/player; there is no workspace-wide
test or VM execution. The earlier shell command bundle is retired; every
assertion lives in the Rust tests listed below.

`tests/gpu_pixels.rs` checks analytical linear mixing, whole-group opacity,
nested masks/clips, transformed path coverage, real Parley/Swash glyphs, image
conversion/retirement, admission, completion, quality fallback and device loss.
`tests/gpu_correctness.rs` checks the 18 draw/clip/join/miter cases, temporary
pressure versus oversize, recursive mask depth boundaries and release on refusal.
Unit tests exercise malformed owning stacks, numeric boundaries, quota accounting,
redraw demand and incompatible/changed surface capabilities. The native window
regression checks completion-triggered retry, accepted zoom geometry, no repeated
permanent preparation, actual device destruction/recovery, stale candidates,
suspend/resume, zero size and shutdown.

Validation on Linux x86_64 uses Mesa llvmpipe 25.0.7/LLVM 19.1.7 (CPU Vulkan
adapter) with RGBA16Float/4x and Xvfb X11 surfaces. This runs real wgpu submission,
readback and surface presentation APIs. All commands above passed: scoped
regressions, strict Clippy, feature-off, ten explicit device tests and the native
window test in both debug/release; real Parley was explicitly checked in both.
Physical GPU, Wayland, Windows/macOS,
mobile OS lifecycle, real hot-unplug and scanout remain unverified. Controlled
suspend/resume and Device::destroy are fault injection, not OS/hardware evidence.
Capability changes are deterministic unit fixtures; native recovery independently
checks fresh capability negotiation on the available adapter.

Logs and screenshots are disposable artifacts under `target/reel-artifacts` or
`/tmp`, never tracked source files. Prior tracked runtime evidence was removed.
The source regressions and commands above provide reproducible evidence.

## Launch

```sh
REEL_DIAGNOSTIC_FONT=/path/to/real-font.ttf \
cargo run --locked -p reel-player --features window --bin reel-diagnostic
```

Drag the shapes, scroll to zoom, F5 destroys/recreates the device, F6 injects
suspend/resume, Escape closes. The optional font includes original Parley glyph
positions rendered by Swash; no reshaping or VM session is involved.
