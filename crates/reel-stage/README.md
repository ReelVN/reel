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
- Fixed route/registry storage and key/view leases use D01 tickets. A raw Arc
  or retired CandidateKey retained outside the bridge stays charged after clear.
  Each lease keeps its original maximum export ticket without a drop/re-reserve
  accounting gap. Key identity and the view guard share that same Arc/ticket;
  no separate identity allocation, second ticket or unbounded identity table is
  created. This conservatively retains scratch/export allowance until the final
  key/view releases. Tests exhaust node,
  bytes, path, fixed storage, candidate slots and aggregate snapshot capacity,
  verify no partial route/publication, and recover after releasing leases.

`cargo test --workspace --features reel-core/test-support --locked --
--test-threads=1` passed 12 D01, 24 stage contracts and the real-dispatch allocation
contract. Direct masonry_core/tree_arena unit checks also passed (5 and 4).
The original real
Button/Slider/Image checks remain: disabled-child ancestor consumption,
Up/Cancel followed by ancestor deletion, QueryCtx/RawCtx frozen geometry,
drag cancellation, DPI, topology/clip/rotation and skin output. New checks cover
first frame/clear, missing query/raw-mut, frozen child order, stashed inclusion,
complete own getters with nonzero insets/baselines, animation Live, view path
differences, capture survival and capacity/recovery.

Review regressions now cover hover/active ancestor callbacks deleting their target
in Live, Presented and the bridge, with debug/release checks. Cursor checks current
liveness after notifications; the existing bounded rewrite loop reconciles surviving
ancestor flags with the selected view, preserving callback invalidations. Disabled
and stashed Layer interception cannot emit actions from Down in Live or Presented;
captured cleanup still receives exactly one Cancel. Shared/mutable safe-tree lookups
no longer allocate ancestor Vecs and preserve descendant scope after reparent/removal.

The recursive lookup introduced in `694b395` has been replaced with bounded path
storage and iterative descent. `check-tree-depth.py` recreates the exact six review
probes using TreeArena<u64>: build on a separate 16MiB stack, print BUILT, query on
the reported small stack, then forget the arena to exclude destruction. It compares
local git snapshots fbb3c79/2534b9a with current source. Old lookup passes all six,
2534b9a overflows all six, and current lookup passes all six with zero allocations
and reallocations (`tree-depth-lookup.log`). The cases are debug find256/find_mut512
on 64KiB, debug find512/find_mut1024 on 128KiB, and release find1024/find_mut2048 on
64KiB. Configured depth128 lookup also passes on both stacks in both profiles.

The arena owns an independent std Mutex<Vec<u64>> workspace, preallocated to
max_path at input configuration. try_lock never blocks; returned handles retain
no lease, and a held/poisoned/unconfigured workspace uses 32 stack slots. no_std
also uses that fallback. There is no new recursive lookup or capacity growth,
and a deep present node remains queryable. Existing event/hover/active routes are
not overwritten. The workspace keeps the arena's Send/Sync traits and is charged
with the five routes by the existing Temporary ticket; no D01 interface changes.
At max_path128 its actual x86_64 overhead is 1,064B, increasing the two-slot
registry+route measurement from 5,304B to 6,368B.

For relative depth D and available capacity C, q=ceil(D/C) chunks visit exactly
P=q*D-C*q*(q-1)/2 parent entries, plus one initial parent query and D child lookups.
Unit tests check this bound. D<=C is linear; a lease fallback uses C=32. For D=2048,
P is 2,098,176 / 66,560 / 17,408 / 2,048 at C=1 / 32 / 128 / 2048. The isolated
100-sample release find/find_mut measurements on a 64KiB stack were:

| Capacity / depth | Parent entries P | find p95/max, µs | find_mut p95/max, µs |
| --- | ---: | ---: | ---: |
| 1 / 2048 | 2,098,176 | 25,308 / 28,102 | 23,914 / 25,765 |
| 32 / 2048 | 66,560 | 1,290 / 1,736 | 803 / 953 |
| 128 / 2048 | 17,408 | 247 / 368 | 247 / 347 |
| 2048 / 2048 | 2,048 | 70.884 / 137.775 | 64.931 / 165.333 |

At configured D=C=128, p95 was 2.3–2.8µs. These cost observations include the
allocator instrumentation; tiny buffers deliberately expose repeated-parent cost.
They do not bound complete Masonry layout/paint/export/drop recursion or guarantee
all depths on every platform. The approved six lookup cases are the verified scope.

The reverse stash/unstash regressions record a hidden snapshot, then live-unstash
before a visible candidate is accepted. Presented Layer interception remains
suppressed; Live immediately resumes. In the real bridge, A is accepted visible,
B is recorded while the Slider or its ancestor is stashed, then live is unstashed
and Down captures under A. Accepting B now sends exactly one Cancel using A and
suppresses further Slider Move/Down/action until a visible C is accepted. Layer
behavior and capture validity walk the selected record's parent links to check
effective hidden state without allocating or re-hit-testing geometry. Normal
move/scale acceptance still retains capture. Both failures before the correction
and debug/release results are in `frozen-*-before-fix.log` and
`frozen-visibility-*.log`; terminal/exit contexts still use the old binding.

The 64-retired-key probe is bounded by the configured pool: at max_candidates=1,
two old keys retain two charged leases, and the remaining 62 record attempts return
SnapshotBudget. Releasing those keys restores recording; key clones do not create
extra tickets. The single-ticket maximum-reservation test also verifies clear,
raw view retention and final release. Before-fix failures and before/after allocation
output are preserved in evidence/.

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
at `694b395` ran with no concurrent builds/tests from this task. Each tree has real Sliders
plus a StageGroup and LayerStack; the exported node count is asserted. Samples
include diagnostic traces: 10 record warmups then 100 cached record/replay/export
samples, and 100 pointer warmups then 1000 captured-drag samples. p95 uses nearest
rank. These are steady-state costs, not cold startup or GPU presentation.

| Controls / actual nodes | Retained geometry bytes | Export p95 range, µs | Export max, µs | Record + replay p95 range, ms | Record + replay max, ms | Pointer p95 range, µs |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 32 / 34 | 11,536 | 1.870–2.619 | 22.002 | 0.029–0.051 | 1.915 | 1.383–1.617 |
| 256 / 258 | 86,800 | 21.296–28.968 | 3,114.033 | 0.534–3.610 | 4.590 | 1.432–1.474 |
| 1024 / 1026 | 344,848 | 120.192–125.530 | 2,189.942 | 5.083–6.528 | 8.314 | 1.433–1.457 |

Captured-pointer maxima were 74.526 / 3,086.681 / 2,054.469µs respectively. Wall-clock tails
vary on this VM. Export p95 stayed below the architecture's starting 1ms capture
quota in these final runs, but 258/1026-node maxima exceeded it. This does not
establish a hard 1ms deadline or low-end/device performance. Complete record/replay
is separately shown; its several-millisecond cost is not hidden in export time.

Temporary child-ID heap peaks were 256 / 2,048 / 8,192 bytes; fixed five route
arrays plus two pending entries charged 5,304 bytes. Snapshot pool peak charges
were 24,320 / 182,016 / 722,688 bytes (two continuously held maximum reservations),
not allocator/RSS peaks. Retained geometry reports actual vector capacities and
view header; host Arc/ticket overhead is charged separately, native stack and
allocator metadata are separate. Whole test process peak RSS, excluding compiler,
was 16,072–17,384 KiB; this includes fonts, CPU rasterization and all fixtures.

At 3 actual nodes / 1,120 geometry bytes / 1,376 snapshot-limit bytes, candidate
slot refusal took 93–99ns, a retained external view's budget refusal 260–638ns,
and release followed by real record recovery 2,138–2,970ns. Final usage was zero.
These tiny timings are diagnostic observations; correctness is asserted separately.
Raw runs are retained in `evidence/release-tests-{1,2,3}.log`.

An additional allocator-instrumented binary dispatches real uncaptured Move events
through PresentationBridge, with 20 warmups and 100 events per case. It hits the
first Slider (reverse painter scan) or misses all controls inside the 1080p group.
On the old code, 1026 nodes allocated 206,400 / 205,200 times and requested
6,604,800 / 6,566,400 bytes over 100 events. The repaired 34/258/1026-node cases
all assert zero allocations and reallocations during warmed whole dispatch. At
1026 nodes p95 was 5.197µs (first-child hit) / 5.035µs (wide miss), with maxima
32.746 / 5.064µs. Frozen AABB scanning is still O(nodes) on a miss; there is no
new spatial index. Native lookup call stacks are separate from the reused heap
route buffers. stats_alloc=0.1.10 is pinned, dev-only, and used only in that test
binary; the normal library and contract timing binary use their usual allocator.

After the frozen-visibility correction, one additional full release run passed
23 stage contracts and the allocation contract (`frozen-visibility-release-tests.log`).
All warmed whole-dispatch cases still allocated/reallocated zero times. At 1026
nodes uncaptured hit/miss p95 was 5.065/4.917µs; export p95/max was 117.144/154.465µs,
record+replay p95/max was 2.060/2.351ms, and captured pointer p95/max was
1.422/29.824µs. Geometry and budget charges were unchanged. This single run does
not replace the three-run baseline above or constitute a device latency guarantee.

After the nonrecursive lookup correction, `tree-depth-release-tests.log` passes
24 contracts and all warmed dispatch cases still assert zero allocations and
reallocations. At 1026 actual nodes uncaptured hit/miss p95 was 5.682/5.552µs;
export p95/max was 126.622/176.352µs, record+replay p95/max was 2.809/5.128ms,
and captured pointer p95/max was 1.910/16.301µs. Geometry remains 344,848B and
snapshot charge remains 361,344B; fixed routes/lookup/registry now charge 6,368B.

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
CARGO_TARGET_DIR="$PWD/target" cargo test --manifest-path third_party/masonry/Cargo.toml -p masonry_core -p tree_arena --lib -- --test-threads=1
CARGO_TARGET_DIR="$PWD/target" cargo clippy --manifest-path third_party/masonry/Cargo.toml -p masonry_core -p tree_arena --all-targets -- -D warnings
cargo test -p reel-stage --release --no-default-features --locked --no-run --message-format=json > /tmp/reel-stage-build.jsonl
python3 crates/reel-stage/scripts/measure.py /tmp/reel-stage-build.jsonl /tmp/reel-stage-release.log
cargo test -p reel-stage --release --no-default-features --locked --test pointer_allocations -- --test-threads=1 --nocapture
python3 crates/reel-stage/scripts/check-tree-depth.py --output /tmp/tree-depth-lookup.log
CARGO_TARGET_DIR="$PWD/target" cargo test --manifest-path third_party/masonry/Cargo.toml -p tree_arena --no-default-features --features safe_tree --lib
cargo tree -p reel-stage --edges normal --locked
PKG_CONFIG_ALLOW_CROSS=1 cargo check -p reel-stage --lib --no-default-features --locked --target i686-unknown-linux-gnu
PKG_CONFIG_ALLOW_CROSS=1 cargo build -p reel-stage --lib --no-default-features --locked --target i686-unknown-linux-gnu
```

Formatting, workspace strict Clippy, direct masonry_core/tree_arena all-target strict Clippy,
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

The host owns an identity base layer wrapper. Fixed upstream paint.rs:295–315
strips its transform while input retains it; the bridge rejects that host mismatch
with UnsupportedRootTransform. The author's stage root is beneath the wrapper
and retains its full transforms; this is an internal host detail. Transformed
child-stage trees are verified. External
placeholders are also explicitly rejected. StageGraph's author NodeId/generation
mapping, resource-version conversion and Scene/blob residency budgets, ExternalDraw,
author image Slider, DSL and GPU backends remain separate tasks. The bridge accounts
geometry and fixed routes/registry, not the complete widget/graphics runtime.

Creator guides were not edited. For the lead's implementation-status update:
the already approved first-frame/clear empty hit policy, accepted-frame geometry,
and immediate disable/stash/removal cancellation now have production evidence.
The Option changes are Rust adaptation APIs; no author syntax/API is introduced.
