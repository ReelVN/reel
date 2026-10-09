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
cargo clippy --locked -p reel-core -p reel-ui -p reel-render -p reel-player --all-targets --all-features -- -D warnings
cargo test --locked -p reel-core -p reel-ui -p reel-render -p reel-player --all-features -- --test-threads=1
cargo test --locked -p reel-core -p reel-ui -p reel-render -p reel-player --all-features --release -- --test-threads=1
cargo check --locked -p reel-core -p reel-ui -p reel-render -p reel-player --no-default-features
cargo test --locked -p reel-core -p reel-ui -p reel-render -p reel-player --no-default-features --features reel-core/test-support -- --test-threads=1
cargo tree --locked -p reel-core -p reel-ui -p reel-render -p reel-player --no-default-features -e normal
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
cargo test --locked -p reel-render --features wgpu --lib white_texture -- --include-ignored --nocapture --test-threads=1
cargo test --locked -p reel-render --features wgpu --lib white_texture --release -- --include-ignored --nocapture --test-threads=1
cargo test --locked -p reel-ui actual_parley_label_glyphs_are_copied_without_reshaping -- --ignored --nocapture
cargo test --locked -p reel-ui --release actual_parley_label_glyphs_are_copied_without_reshaping -- --ignored --nocapture
cargo test --locked -p reel-player --features window actual_window_retry_and_recovery_contract -- --ignored --nocapture --test-threads=1
cargo test --locked -p reel-player --features window --release actual_window_retry_and_recovery_contract -- --ignored --nocapture --test-threads=1
```

These commands select only core/UI/render/player; there is no workspace-wide
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

## Issue #36 preparation-local white texture verification

Base: [`9e0e10b856a1a6995da0a9268b605cd508e3ad71`](https://github.com/jhq223/reel/commit/9e0e10b856a1a6995da0a9268b605cd508e3ad71).
Validated Rust source:
[`28b5d98c32ebbd7d86c60186e944cfbd3a18ada1`](https://github.com/jhq223/reel/commit/28b5d98c32ebbd7d86c60186e944cfbd3a18ada1).
The following report commit changes no validated Rust. Scope is the existing
reel-render GPU Context, its private test instrumentation and R02 documents.
No new crate, dependency, public API, quality negotiation or other resource
cache/atlas change. reel-ui keeps its current single-package module structure.
R03 production pipeline construction, locks, error scopes and publication are
unchanged; two test-fixture modules are now shared with the new tests. Index and
both creator guides remain for the owner to synchronize. No V03 executor or
Aside source program was executed.

Context starts with no white image and reserves/uploads it on first solid or
clip use. Its slot is shared by both crossfade streams and recursive masks.
Only encoder/queue references and the original budget tickets survive Context;
Renderer/Gpu/Pipelines retain no white image. The existing upload cost remains
536 resource bytes: 16 f32 input + 256 CPU padded staging + 256 GPU staging +
8 texture extent. It is charged once for a using transaction. All target,
transient and other resource paths retain their original accounting.

`src/gpu/white_texture_tests.rs` has one extent-accounting unit test and seven
explicit real-device tests. Test-build-only counters live on each Gpu and
increment beside actual resource `create_texture`, staging `create_buffer_init`
and encoder `copy_buffer_to_texture` calls. They count uploads, not cache hits;
targets, bind groups, SurfacePass and diagnostic readback are excluded.

| Transaction | Actual texture / staging / copy calls | Resource bytes |
| --- | ---: | ---: |
| Empty group, no white use | 0 / 0 / 0 | 0 |
| 100 solid fills | 1 / 1 / 1 | 536 |
| 100 geometric clips | 1 / 1 / 1 | 536 |
| 50 solids + 50 clips | 1 / 1 / 1 | 536 |
| Crossfade, 100 uses in each input | 1 / 1 / 1 | 536 |
| Solid plus recursive mask with 50 solids + 10 clips | 1 / 1 / 1 | 536 |
| 1x1 image only, white unused | 1 / 1 / 1 (image) | 536 |
| That image + 100 solids | 2 / 2 / 2 (image + white) | 1,072 |

Crossfade readback asserts every pixel is linear premultiplied
`[0.75, 0, 0.25, 1]`; recursive alpha-mask and mixed-image readbacks assert white.
One 536-byte cap admits all 100 solids; a second live transaction gets resource
Pressure without uploads and preserves the first's exact usage/admission.
535 bytes gives permanent Budget before any upload. Unsupported record and
second-crossfade-input failures after white creation, and later target/transient
Budget failures, return all partial reservations. Dropping an unsubmitted
Prepared releases its ticket, and the next transaction uploads anew.

Eight concurrent Contexts each upload once and hold exactly 8 resource tickets /
4,288 resource bytes; the ninth is Busy. Actual queue completion is deliberately
observed by Device::poll(Wait) before Renderer::poll: all frame leases, residency
and admission remain live during this observation delay. Renderer::poll retires
all eight IDs once and returns CPU frame and GPU usage to zero. Foreign submit,
actual Device::destroy before submit/admission, and destroy/drop with submitted
work release or retain obligations at the existing terminal boundaries; a fresh
same-generation renderer uploads independently. Retained budget-pool handles
assert all bytes/tickets are zero after Renderer drop.

All reproducible commands above passed. In both debug/release the scoped
all-feature suite passed 87 tests with 32 ignored (ignored is not passed).
Explicit runs passed eight white-texture tests, seven original GPU pixel tests,
three correctness tests, ten R03 pipeline tests, one actual Parley test and one
Xvfb/X11 window recovery test per build. Parley preserves 14 positioned glyphs;
Swash readback covers 593 pixels. The existing analytical composition, whole-group
opacity, clips/masks, image conversion/retirement, quality tiers and recovery
assertions remain. fmt and four-package strict Clippy passed. Clippy initially
reported duplicate test fixture modules; sharing the existing modules resolves
that failure without suppressing the lint or changing R03 assertions.
Feature-off check/tree passed, with neither wgpu, winit, aside-vm, aside-compiler
nor retired UI packages in the normal dependency tree. Feature-off tests passed
75 tests with four ignored in each build, using the existing test-support gate:

```sh
cargo test --locked -p reel-core -p reel-ui -p reel-render -p reel-player --no-default-features --features reel-core/test-support --release -- --test-threads=1
```

### Same-load count, byte and CPU comparison

Measurements use the same immutable Rust Scene/FrameBuild fixture and test counters
in a detached worktree at the fixed base, preserving its production draw,
Context and pipeline code. Only test instrumentation, the measurement source,
module registration and fixture visibility are added there. The new count
regression fails on the old path with actual `[100,100,100]` versus `[1,1,1]`
(expected debug exit 101). The benchmark's baseline expectation is 128; every
sample asserts all three actual call deltas and resource usage, and final
readback asserts unchanged white pixels. The benchmark runs on the fixed base
and the validated source with the commands below in both debug and release:

```sh
# Fixed baseline worktree with identical test instrumentation/fixture only:
cargo clean -p reel-render
cargo clean -p reel-render --release
REEL_R04_EXPECT_UPLOADS=128 cargo test --locked -p reel-render --features wgpu --lib white_texture_cpu_measurement -- --ignored --nocapture --test-threads=1
REEL_R04_EXPECT_UPLOADS=128 cargo test --locked -p reel-render --features wgpu --lib white_texture_cpu_measurement --release -- --ignored --nocapture --test-threads=1
# Validated source, default expected upload count is one:
cargo clean -p reel-render
cargo clean -p reel-render --release
cargo test --locked -p reel-render --features wgpu --lib white_texture_cpu_measurement -- --ignored --nocapture --test-threads=1
cargo test --locked -p reel-render --features wgpu --lib white_texture_cpu_measurement --release -- --ignored --nocapture --test-threads=1
```

Both loads have 128 white uses at 64x48, RGBA16Float/4x, default immutable limits.
Each has one warm-up preparation and 30 timed prepares on the same Renderer.
Pipeline/device construction, frame building, Prepared drop, submission,
completion and readback are outside the prepare timer. Runs are sequential with
one test thread and no parallel build. Sorted nearest-rank p50/p95 use indices
14/28. These are instrumented native Rust builds; driver/OS caches are not flushed.
Measured resource extents fall from 68,608 to 536 bytes in every sample (68,072
bytes / 99.21875% removed); actual upload object/copy calls fall from 128 to one.
The budget formula includes the same four extents on both sides, not driver
allocation granularity or private object/pipeline storage.

The worktrees share a target directory; profile-specific reel-render cleanup
above forces recompilation when switching source. Initial baseline release
attempts reused an optimized artifact and failed the actual-call assertion
`[1,1,1] != [128,128,128]`. They produced no accepted timing distribution.
The final baseline and optimized runs verify their own expected counts before
any measurements are accepted. Cleanup/build time is outside the timer.

CPU durations in microseconds:

| Build | Load | Baseline p50 | Baseline p95 | Shared p50 | Shared p95 |
| --- | --- | ---: | ---: | ---: | ---: |
| debug | 128 solids | 53951.985 | 62965.831 | 40613.357 | 66182.034 |
| debug | 64 solids + 64 clips | 170995.179 | 209479.823 | 140187.925 | 148207.908 |
| release | 128 solids | 3113.678 | 3765.699 | 2067.323 | 2893.617 |
| release | 64 solids + 64 clips | 9983.722 | 11876.032 | 6687.901 | 9703.472 |

All four p50 values decrease in these runs. Debug solid-only p95 increases;
these 30-sample distributions do not establish a uniform tail-latency gain or
engine-wide latency bound. Target creation, mesh/bind-group and scope work are
unchanged by this optimization.

Environment: Debian 13 x86_64, Rust 1.96.0 (ac68faa20), AMD EPYC 9V74 virtual CPU
with five visible CPUs, Mesa llvmpipe 25.0.7-2+deb13u1 / LLVM 19.1.7 CPU Vulkan,
Xvfb/X11 1280x800x24, DejaVuSans.ttf. Rust and native development/driver packages
are installed/extracted under `/workspace`, without repository dependency or
Cargo.lock changes. The reproducible environment is
`target/reel-artifacts/issue36/env.sh`, using WGPU_BACKEND=vulkan,
the extracted lvp_icd.json, DISPLAY=:99 and REEL_R01_TEST_FONT=DejaVuSans.ttf.

These CPU/extent numbers describe this software-device preparation workload.
They do not measure physical GPU execution/throughput, scanout or end-to-end
frame latency. Physical GPU, hardware OOM/hot-unplug, Wayland, Windows/macOS,
mobile OS lifecycle and asynchronous WebGPU scope completion remain unverified.
Device::destroy and suspend/resume are controlled injections. Driver-private
allocation/latency and other resource optimizations remain outside this task.
There is no remaining local validation blocker; owner review/acceptance remains.
The PR remains draft and Issue #36 remains open.

Generated command/exit records, complete test/benchmark logs and the environment
remain under ignored `target/reel-artifacts/issue36/` (`commands.txt`,
`results.txt`, `validated-source.txt`, `white-*.log`, GPU/pipeline/window logs,
`baseline-red-debug.log`, `baseline-measure-*.log`, `optimized-measure-*.log`).
No generated evidence is tracked. Normal pushes use `[skip ci]` commits; no
Actions enable/dispatch/rerun, force push, main write, merge or ready operation.

## Launch

```sh
REEL_DIAGNOSTIC_FONT=/path/to/real-font.ttf \
cargo run --locked -p reel-player --features window --bin reel-diagnostic
```

Drag the shapes, scroll to zoom, F5 destroys/recreates the device, F6 injects
suspend/resume, Escape closes. The optional font includes original Parley glyph
positions rendered by Swash; no reshaping or VM session is involved.
