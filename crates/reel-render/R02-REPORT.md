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

The correctness follow-up did not add persistent caches/atlas, scene-wide quality
selection, white-texture or pipeline reuse. Issue #32 adds only the bounded
Renderer-local Pipelines reuse described below. Unsupported records and target
limitations in the interface remain required full-engine work.

## Reproducible checks

Run from the repository root; use Rust 1.96 and native Freetype/Fontconfig metadata:

```sh
cargo fmt --all -- --check
cargo clippy --locked -p reel-core -p reel-stage -p reel-render -p reel-player --all-targets --all-features -- -D warnings
cargo test --locked -p reel-core -p reel-stage -p reel-render -p reel-player --all-features -- --test-threads=1
cargo test --locked -p reel-core -p reel-stage -p reel-render -p reel-player --all-features --release -- --test-threads=1
cargo check --locked -p reel-core -p reel-stage -p reel-render -p reel-player --no-default-features
cargo test --locked -p reel-core -p reel-stage -p reel-render -p reel-player --no-default-features -- --test-threads=1
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
cargo test --locked -p reel-render --features wgpu --lib pipeline_reuse -- --ignored --nocapture --test-threads=1
cargo test --locked -p reel-render --features wgpu --lib pipeline_reuse --release -- --ignored --nocapture --test-threads=1
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

## Issue #32 pipeline reuse verification

Base: `7990c326f1acf3531a940134eab38bd55c66f214`. Scope is reel-render's private
GPU preparation path and this R02 contract/report; no public API, dependencies,
quality selection, player behavior or VM execution changed. The source tests
live in `src/gpu/pipeline_tests.rs`; counters and the invalid-shader construction
fixture are test-build-only. Counters increment at the actual wgpu device calls,
not at lookups or quality changes. SurfacePass construction is excluded.
Validated source/contract commit:
[`2809d0ce5851a7dd2a4aab2363ec448e6aba3f1d`](https://github.com/jhq223/reel/commit/2809d0ce5851a7dd2a4aab2363ec448e6aba3f1d).
The following report-only commit changes no validated Rust source. No V03
executor or Aside source program was executed.

Ten explicit real-device tests verify:

- One Renderer selects all three tiers through different physical sizes with
  one immutable 4,800,000-byte target budget. Exactly 100 preparations per tier
  (one cold, 99 warm) create one complete set per tier. Cumulative counts
  `[bind layouts, shaders, pipeline layouts, render pipelines, samplers]` are
  `[2,2,2,2,1]`, `[4,4,4,4,2]`, `[6,6,6,6,3]`. Returning to each tier,
  changing only the degraded label and pixel readback create no further objects.
  The unused slots remain empty until first use.
- Eight simultaneous cold preparations create one set, retain eight admission
  tickets, and refuse the ninth as Busy. Dropping them returns all usage to zero.
- Different Renderer/Gpu instances with the same generation create distinct
  sets; foreign submit and stale readback are rejected. Actual device destruction
  stops warm admission; a replacement starts empty and independently renders.
- A deliberately invalid WGSL constructor exercises actual shader/pipeline
  failures, publishes no slot and releases admission. Sticky faults prevent
  repeated construction. A background uncaptured driver-error callback after
  warming refuses prepare, submit and readback without further construction.
- A caller's outer Validation scope cannot hide failed construction: the
  preparation returns sticky Driver, zero slots are published, turning off the
  invalid-shader fixture cannot resurrect the attempt, and the outer scope pops
  empty. Valid cold/warm preparation preserves the caller's previously captured
  raw-device error and the original pixels. Concurrent construction of three
  distinct slots balances the outer stack; a concurrent failing batch attempts
  exactly one set before sticky refusal stops the other tiers.
- Shared Pressure preserves the exact preexisting usage; permanent unsupported
  records and target/resource/transient/readback Budget errors release partial
  reservations. Valid pipeline slots survive these refusals. Crossfade readback
  retains the analytical pixels. Drop Prepared and Renderer return usage to zero.
- Actual queue completion keeps CPU frames, all residency and admission live
  until Renderer::poll observes completion once; after observation frame-pool
  bytes and target/resource/transient usage are zero despite the warm slot.
- Paired cold/warm CPU distributions use the same immutable one-solid-rectangle
  frame, physical size and quality in each pair, with 30 fresh Renderer/devices
  per tier. Device creation, Prepared drop, readback, submission and completion
  are excluded from the timed prepare call. Percentiles use nearest rank on
  sorted durations (p50 index 14, p95 index 28). Cold means first use of that
  Renderer; OS/driver shader caches are not flushed.

Independent review of frozen `75a2d0db9fc43593c46885f7b6592d0382912635` found
that an outer caller Validation scope could consume constructor errors before
the uncaptured handler, so Gpu::check alone did not prove success. The new Rust
regression reproduced this on that implementation (debug exit 101). Constructor
scopes now capture Validation/Internal/OutOfMemory, pop completely in reverse
order, record errors into the existing sticky Driver state and confirm success
before publication. A single Renderer-owned construction lock covers all tiers'
push/create/pop transactions. Warm lookup/replay stays outside that lock.
Native wgpu 26.0.1 `src/backend/wgpu_core.rs` returns `ready(scope.error)` on pop:
one future poll confirms completion without `device.poll(Wait)` or a queue wait.
A Pending confirmation is a permanent Device refusal with no published slot;
WebGPU Promise behavior is not treated as native-ready or verified here.
The public raw device permits outer scopes. Callers must pair and coordinate
their own stack mutations, avoiding interleaving with construction or other
scope transactions. Unrelated errors captured by callers remain theirs; valid
constructor scopes do not drain them. This boundary and the synchronous-backend
limit are explicit in R02-INTERFACE. Public APIs and admission limits are intact.

All commands in the reproducible checks above passed on this source. The scoped
all-feature suite passed 68 tests in each debug/release build; its 23 ignored
tests are not counted as passes. Explicit runs passed ten pipeline tests,
seven original gpu_pixels tests, three original gpu_correctness tests, one real
Parley test and one native window test in each debug/release build. The additional
feature-off test run passed 45 tests (two ignored); feature-off check and normal
dependency tree passed, with no wgpu, winit, aside-vm or aside-compiler in that
tree. fmt and strict Clippy (`--all-targets --all-features -- -D warnings`) passed.

Environment: Debian 13 x86_64, Rust 1.96.0 (ac68faa20), AMD EPYC 9V74 virtual CPU
(five visible CPUs), Mesa llvmpipe 25.0.7-2+deb13u1 / LLVM 19.1.7 CPU Vulkan,
Xvfb/X11 1280x800x24 and DejaVuSans.ttf. Rust and missing native development/driver
packages were installed/extracted under `/workspace`; repository dependencies
and Cargo.lock are unchanged. The actual environment selected
`WGPU_BACKEND=vulkan`,
`VK_DRIVER_FILES=/workspace/native-deps/root/usr/share/vulkan/icd.d/lvp_icd.json`,
`DISPLAY=:99`, and
`REEL_R01_TEST_FONT=/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf`.
CPU distributions below are from test-instrumented native Rust builds, run after
the other checks completed, with one test thread and no parallel build. Units
are microseconds; each row is 30 paired cold/warm samples at the named tier.

| Build | Format/samples | Physical size | Cold p50 | Cold p95 | Warm p50 | Warm p95 |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| debug | Rgba16Float/4x | 400x300 | 9585.536 | 11159.863 | 534.874 | 784.119 |
| debug | Rgba16Float/1x | 800x600 | 9216.070 | 10203.255 | 509.706 | 737.439 |
| debug | Rgba8Unorm/1x | 1000x800 | 10879.983 | 12690.055 | 510.455 | 810.189 |
| release | Rgba16Float/4x | 400x300 | 4379.414 | 5601.160 | 32.068 | 35.123 |
| release | Rgba16Float/1x | 800x600 | 4322.559 | 4845.485 | 30.475 | 89.675 |
| release | Rgba8Unorm/1x | 1000x800 | 5965.365 | 7724.885 | 39.711 | 78.138 |

Every sample pair asserts exactly `[2,2,2,2,1]` real Pipelines creations and zero
application usage after dropping each preparation. In the separate 100-repeat
test, the release first-cold times were 6032.199 / 2093.558 / 3580.393 us and the
99-warm p50/p95 values were 23.805/57.296, 19.239/36.134, 20.260/30.446 us in tier
order. These single cold samples are not cold distributions. Counts remained
one set per tier and three sets total in both build modes.

The measurements show prepare CPU cost on this software Vulkan environment;
they do not measure physical GPU throughput, GPU execution time, frame latency
or driver-private pipeline memory. Physical GPUs, Wayland, Windows/macOS,
mobile OS lifecycle, real hot-unplug and scanout remain unverified. Device::destroy,
background validation errors and suspend/resume calls are controlled injections.
No remaining local validation blocker; independent owner review/acceptance is
still required. The PR stays draft and Issue #32 stays open. Document index and
both creator guides remain for the owner to synchronize.

Generated command/exit records, complete test output and environment setup stay
under ignored `target/reel-artifacts/issue32/scope-fix` (`results.txt`,
`red-debug.log`, `pipeline-debug.log`, `pipeline-release.log` and GPU/window logs;
the shared environment setup is `target/reel-artifacts/issue32/env.sh`). No generated evidence is
tracked. The repository has no workflow files; normal branch push uses
`[skip ci]` commits, with no Actions enable/dispatch/rerun operation.

## Launch

```sh
REEL_DIAGNOSTIC_FONT=/path/to/real-font.ttf \
cargo run --locked -p reel-player --features window --bin reel-diagnostic
```

Drag the shapes, scroll to zoom, F5 destroys/recreates the device, F6 injects
suspend/resume, Escape closes. The optional font includes original Parley glyph
positions rendered by Swash; no reshaping or VM session is involved.
