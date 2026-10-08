# U02 production geometry and acceptance bridge

Implements Issue #11 against architecture §15 at `31b9430`, with D01
`c03226a` identities/budgets. Draft PR #12 targets PR #7's
`task/issue-3-masonry-contract`; this branch combines both dependencies locally.
Integrate PR #7 and D01 before moving this task onto the shared production base.
No remote main update, merge or Actions invocation is part of this task.

The bridge records a real Masonry rewrite/layout/compose/paint cycle, replays
its complete VisualLayerPlan into an owned Imaging Scene and pairs it with that
cycle's compact geometry. Only an exact renderer notice can install the view.
This is a host acceptance bridge, not StageGraph or a GPU renderer. The exact
public APIs, limits and ownership are in [INTERFACE.md](INTERFACE.md).

## Production changes and verification

- All registered nodes, including stashed ones, enter contiguous geometry,
  child-index and ID-index arrays. No WidgetState, widget/property cache or
  per-node ancestor vector is copied. Export walks borrowed direct arena children
  rather than allocating safe TreeArena lookup paths at every node. Registered
  node/byte bounds are checked before that walk; child-ID scratch heap peak and
  maximum depth are also bounded before allocating the final arrays.
- Every own context validates its selected record before calling widget code;
  queries/raw mutable queries return Option for absent children. WidgetRef
  enumerates selected painter topology, filtering destroyed children. Ordinary
  animation/layout/mutation and nonpointer updates use Live. The context literal
  inventory in evidence/ is a review aid, not a substitute for source review.
- Pointer routing shares one fixed borrowed view and a reused bounded bubble
  route. Old-only exits use old geometry; new-only entries and stationary cursor
  use the new view. Shared ancestors receive no duplicate notifications. Current
  disabled/stashed/liveness stay live; disabled hits still allow parent handling.
- Capture survives valid acceptance. Clear, epoch replacement, disable, stash,
  removal and loss of focus cancel invalid capture once. Up/Cancel mark delivery
  before entering the captured handler, which still sees capture as active;
  ancestor deletion afterward does not deliver a second terminal callback.
- FrameId, separately reported SurfaceEpoch, acceptance ordering and opaque
  candidate identity are checked. Wrong/foreign/duplicate notices cannot accept
  a frame; skipped/unavailable/error notices retire only their candidate.
- Fixed route/registry storage and geometry leases use D01 tickets. A raw Arc
  retained outside the bridge stays charged after clear. Tests exhaust node,
  bytes, path, fixed storage, candidate slots and aggregate snapshot capacity,
  verify no partial route/publication, and recover after releasing leases.

`cargo test --workspace --features reel-core/test-support --locked --
--test-threads=1` passed the 12 D01 and 18 stage contracts. The original real
Button/Slider/Image checks remain: disabled-child ancestor consumption,
Up/Cancel followed by ancestor deletion, QueryCtx/RawCtx frozen geometry,
drag cancellation, DPI, topology/clip/rotation and skin output. New checks cover
first frame/clear, missing query/raw-mut, frozen child order, stashed inclusion,
complete own getters with nonzero insets/baselines, animation Live, view path
differences, capture survival and capacity/recovery.

The actual production candidate is also CPU-rendered in the nested-group case:
two 0.5-opacity groups yield alpha 64 over overlapping Image and Button. Group
and clip scope crosses three fragments, two of which fail independent validation;
complete replay validates and matches the original pixels. Cached layer mode
survives pre-only/post-only repaint without rerunning the declaring paint callback.

One new regression initially failed: live stashing left the Slider captured.
`stashed-before-fix.log` preserves that failure. update.rs now requests pointer
reconciliation when stashed state changes; final debug/release runs pass.

## Cost observations

2026-10-08; Linux x86_64 KVM container, Intel Xeon Platinum 8573C, Rust 1.96.0,
release `opt-level=3`, no default features, 1920×1080. Three consecutive processes
ran with no concurrent builds/tests from this task. Each tree has real Sliders
plus a StageGroup and LayerStack; the exported node count is asserted. Samples
include diagnostic traces: 10 record warmups then 100 cached record/replay/export
samples, and 100 pointer warmups then 1000 captured-drag samples. p95 uses nearest
rank. These are steady-state costs, not cold startup or GPU presentation.

| Controls / actual nodes | Retained geometry bytes | Export p95 range, µs | Export max, µs | Record + replay p95 range, ms | Record + replay max, ms | Pointer p95 range, µs |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 32 / 34 | 11,536 | 1.885–1.899 | 52.116 | 0.029–0.036 | 0.060 | 1.568–1.590 |
| 256 / 258 | 86,800 | 18.780–22.354 | 55.160 | 0.506–0.670 | 2.273 | 1.579–1.597 |
| 1024 / 1026 | 344,848 | 118.296–936.847 | 2,475.236 | 2.586–5.179 | 6.898 | 1.592–1.666 |

Pointer maxima were 44.821 / 29.162 / 2,056.281µs respectively. Wall-clock tails
vary on this VM. Export p95 stayed below the architecture's starting 1ms capture
quota in these final runs, but the 1026-node maximum exceeded it. This does not
establish a hard 1ms deadline or low-end/device performance. Complete record/replay
is separately shown; its several-millisecond cost is not hidden in export time.

Temporary child-ID heap peaks were 256 / 2,048 / 8,192 bytes; fixed five route
arrays plus two pending entries charged 5,304 bytes. Snapshot pool peak charges
were 23,776 / 177,888 / 706,272 bytes (accepted lease plus next export reservation),
not allocator/RSS peaks. Retained geometry reports actual vector capacities and
view header; host Arc/ticket overhead is charged separately, native stack and
allocator metadata are separate. Whole test process peak RSS, excluding compiler,
was 15,772–17,660 KiB; this includes fonts, CPU rasterization and all fixtures.

At 3 actual nodes / 1,120 geometry bytes / 1,376 snapshot-limit bytes, candidate
slot refusal took 84–100ns, a retained external view's budget refusal 315–336ns,
and release followed by real record recovery 3,054–3,685ns. Final usage was zero.
These tiny timings are diagnostic observations; correctness is asserted separately.
Raw runs are retained in `evidence/release-tests-{1,2,3}.log`.

## Reproduction and boundaries

Linux prerequisites: the pinned rust-toolchain, Fontconfig development libraries
and pkg-config. This runner used official Debian packages extracted outside the
repository, with FONTCONFIG_DYNAMIC=1, PKG_CONFIG_SYSROOT_DIR and PKG_CONFIG_PATH
pointing to their lib/pkgconfig **and** share/pkgconfig directories. No system
packages or binaries are vendored in this task.

```sh
cargo fmt --all --check
cargo fmt --manifest-path third_party/masonry/Cargo.toml --all --check
cargo test --workspace --features reel-core/test-support --locked -- --test-threads=1
cargo clippy --workspace --all-targets --features reel-core/test-support --locked -- -D warnings
CARGO_TARGET_DIR="$PWD/target" cargo clippy --manifest-path third_party/masonry/Cargo.toml -p masonry_core --lib --no-default-features -- -D warnings
cargo test -p reel-stage --release --no-default-features --locked --no-run --message-format=json > /tmp/reel-stage-build.jsonl
python3 crates/reel-stage/scripts/measure.py /tmp/reel-stage-build.jsonl /tmp/reel-stage-release.log
cargo tree -p reel-stage --edges normal --locked
PKG_CONFIG_ALLOW_CROSS=1 cargo check -p reel-stage --lib --no-default-features --locked --target i686-unknown-linux-gnu
PKG_CONFIG_ALLOW_CROSS=1 cargo build -p reel-stage --lib --no-default-features --locked --target i686-unknown-linux-gnu
```

Formatting, workspace strict Clippy, direct modified masonry_core strict Clippy,
debug/release contracts and i686 check/rlib build passed. The broader direct vendored
workspace `clippy --workspace --lib ... -D warnings` finds seven existing warnings
in fixed upstream Button/Label/Slider (single_match and narrowing casts). Their
behavior was preserved; `selected-upstream-clippy.log` records that failed optional
check. Normal dependencies include one local masonry_core/tree_arena and reel-core;
they contain no winit/wgpu, reel-render/player or registry Masonry duplicate.
Masonry itself has no reel-core dependency.

i686 evidence includes a Rust rlib build whose object is ELF 32-bit Intel 80386,
using host Fontconfig pkg-config metadata. It does not include 32-bit executable/
native-library linking or execution. No Vita, Windows/N100 or GPU device acceptance
is claimed.

The fixed upstream source closure, original hashes, Apache-2.0 licenses and
complete local diff are under third_party/masonry. To audit, extract official
Xilem commit `271a27a6d4a930f7878d404f9014e3c50a3a9b88`, then run:

```sh
python3 crates/reel-stage/scripts/audit-source.py /path/to/extracted-upstream
```

The audit verifies 94 original source/build/doc/resource files and the one new
geometry source, exact diff and absence of nested Git. Only four required crates,
Button/Image/Slider and Button's Label dependency plus their properties/theme
are retained.

An identity base layer root is required. Fixed upstream paint.rs:295–315 strips
its transform while input retains it; the bridge rejects this mismatch with
UnsupportedRootTransform. Transformed child-stage trees are verified. External
placeholders are also explicitly rejected. StageGraph's author NodeId/generation
mapping, resource-version conversion and Scene/blob residency budgets, ExternalDraw,
author image Slider, DSL and GPU backends remain separate tasks. The bridge accounts
geometry and fixed routes/registry, not the complete widget/graphics runtime.

Creator guides were not edited. For the lead's implementation-status update:
the already approved first-frame/clear empty hit policy, accepted-frame geometry,
and immediate disable/stash/removal cancellation now have production evidence.
The Option changes are Rust adaptation APIs; no author syntax/API is introduced.
