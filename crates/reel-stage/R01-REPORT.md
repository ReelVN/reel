# R01 result — portable frames and render interfaces

Issue #25 is implemented against the complete architecture §16 contract at
`ac00418231ddc16ec0b90b2181cd77138b3f6e49`, extending independently checked U02
`8dc242891be199ef2e6da7bc88a6ffdaf80cc403`. The production path moves one real
Masonry plan and matching geometry/key, compiles public imaging 0.0.1 records
without a second Scene clone/flatten, and publishes only a complete immutable
frame. Export/upload completion leaves input on the last accepted geometry.

The review boundaries are [stage/frame.rs](src/frame.rs),
[stage/resources.rs](src/resources.rs), [stage/lib.rs](src/lib.rs),
[render/lib.rs](../reel-render/src/lib.rs) and
[render/color.rs](../reel-render/src/color.rs). The small licensed Masonry fork
change covers External order/token, reliable group owner/effect metadata and
full forward origin/scroll transforms. [R01-INTERFACE.md](R01-INTERFACE.md)
specifies ownership, quotas, cancellation and the portable profile;
[reel-render/README.md](../reel-render/README.md) specifies backend consumers.

## Behavior evidence

The full debug/release workspace runs pass 12 core contracts, 24 original U02
contracts, the real pointer allocation contract, ten R01 portable-frame cases
and four render-consumer cases. The separately invoked real Parley Label case
passes in both profiles: one run, 14 positioned glyphs, unchanged glyph IDs,
positions, font index, size and variation coordinates. See
[debug](evidence/r01/debug-tests.log),
[release](evidence/r01/release-tests.log),
[final render debug](evidence/r01/render-debug-tests.log),
[Parley debug](evidence/r01/parley-debug.log) and
[Parley release](evidence/r01/parley-release.log).

These checks establish the following behavior, with actual Masonry records:

- [R01 cases](tests/r01.rs): groups/clips spanning fragments; retained External
  `pre → own clip → external → children → pop clip → post` sequence, pre-only
  cached repaint and explicit Inline reset. Base/child/External transforms match
  frozen geometry under nested scrolling, nonzero padding and finite singular
  matrices, allowing floating-point association roundoff below 1e-12 per affine
  coefficient. U02's original transformed-root CPU pixel/hit case now agrees.
- Local Scene IDs remain source-scoped; repeated mask uses share a local
  definition with distinct transforms. Nested masks have independent bounded
  stacks. The portable profile rejects crossed clip/group lifetimes with the
  owning widget and fragment/command location; ordinary cross-fragment nesting
  remains valid. Nonfinite data, depth, byte and work exhaustion abort publication.
  Ink bounds cover a thin stroke's AA fringe at DPI 1.0 and 0.5; finite geometry
  whose transformed bounds overflow remains conservatively Unbounded. Local
  mask sampling and unknown Gaussian/glyph/filter extents require target clamping.
- A large record resumes within path elements, glyphs, variation coordinates,
  gradient stops, stroke dashes and long UTF-8/context strings. Mapping/probe and
  allocation work is prepaid; each slice stays within its command/unit/byte
  allowance. Cancellation, drop, failed presentation and changed SurfaceEpoch
  invalidate candidate eligibility. Successful build and upload never accept it.
- [Render cases](../reel-render/tests/contracts.rs): duplicate image draws use
  one prepared version; retirement during a paused build denies new acquisition
  and registration of the still-leased retired version, while existing upload,
  build and frame leases remain usable. Old/new versions coexist; final frame
  completion and lease release precede backend retirement and slot generation
  reuse. Owned Blob capacity remains charged through the final producer cache
  alias, including after registry drop. Rejected uploads return the original
  request; cancellation has one terminal result and restores live byte/ticket
  counts. Null presentation skips without installing input geometry.
- Linear analytical expectations independently check 0.5 sRGB/0.5 alpha yielding
  0.10702057024111628 premultiplied linear RGB, luminance mask coefficients,
  whole-group alpha applied once, nested 0.5 groups and crossfade. The existing
  CPU replay alpha/geometry cases are retained as structural references. They
  do not establish linear RGB rasterization.

All warmed U02 whole-pointer dispatch cases still assert zero allocations and
reallocations. Direct masonry_core/tree_arena debug and release checks pass five
and four cases; strict all-target Clippy passes for both selected crates.
[Source audit](evidence/r01/source-audit.log) verifies 94 fixed upstream files,
one existing local geometry source and the exact 206,315-byte production diff.
Original hashes, copyright headers and Apache-2.0 license are retained.

## Export and source residency observations

2026-10-08, Rust 1.96.0, Linux x86_64 KVM, Xeon Platinum 8573C, five exposed CPUs;
details and native Fontconfig setup are in [environment.log](evidence/r01/environment.log).
Two isolated processes per workload ran without concurrent builds/tests from
this task. Each uses actual Masonry recording at 400×300, release opt-level=3,
stats_alloc 0.1.10 and slices of two commands / 128 units / 4,096 byte work.
Path workload: 16 paths, each with one move and 2,048 line elements. Warm samples
are 20 complete record/build/drop cycles per process, with nearest-rank p95.
The full-build timer includes setup, **every** step and finish/source cleanup;
the first slice and maximum slice are separately observed.

| Observed time | 512 rectangles | 16 large paths |
| --- | ---: | ---: |
| Cold real record | 0.434–0.707 ms | 1.811–1.820 ms |
| Cold complete setup/export/finish | 0.568–0.707 ms | 3.612–3.831 ms |
| Warm complete build p50 | 0.350–0.390 ms | 2.580–2.668 ms |
| Warm complete build p95 | 0.475–0.515 ms | 2.859–2.873 ms |
| Warm complete build maximum | 0.519–0.617 ms | 2.958–3.020 ms |
| Maximum step, including capacity reservations | 24.938–42.153 µs | 56.271–66.163 µs |
| Finish, including frozen-source cleanup | 21.012–24.552 µs | 4.387–4.427 µs |
| Paused cancellation | 2.734–3.245 µs | 2.602–2.695 µs |
| Isolated RecordedSource drop, cache retained | 2.632–2.826 µs | 2.203–3.121 µs |
| Final producer/widget/cache drop | 48.933–62.932 µs | 13.473–13.575 µs |

Cold builds use 258 / 901 steps and respectively 11 / 27 allocation calls,
303,640 / 1,857,816 newly requested bytes, with **zero compiler reallocations**.
The path copies account for 16 allocations and 1,835,904 bytes; rectangle steps
allocate nothing after setup. Source recording is separate: 14 / 46 allocations
and 17 / 7 reallocations, requesting 551,996 / 3,696,060 bytes. The probe measures
these actual allocator operations, rather than guessing private Scene tables
from public command lengths.

| Actual live allocator-request bytes above pre-fixture baseline | Rectangles | Paths |
| --- | ---: | ---: |
| Producer before record | 287,516 | 1,861,532 |
| Recorded source and producer cache | 838,416 | 5,556,496 |
| Paused source plus partial output | 1,141,760 | 5,578,112 |
| Completed build before source release | 1,141,760 | 7,414,016 |
| Immutable frame after source release | 856,304 | 5,558,512 |
| Frame dropped; producer cache retained | 561,416 | 3,709,448 |
| Paused candidate cancelled | 561,416 | 3,709,448 |
| Producer cache dropped | 712 | 712 |

The final 712 bytes are probe/pool/sample bookkeeping still in scope. Source
Drop releases 277,000 / 1,847,048 requested bytes in seven / 23 deallocations;
finish also drops builder scratch while transferring the matching geometry/key
lease to the immutable frame. Frame
accounting conservatively retains 304,800 / 1,858,976 bytes against the original
16MiB maximum output reservation until its final clone drops. Output pool live
usage returns to zero after frame drop even though the measured **producer
cache remains resident**. These are distinct accounting domains.

Raw allocator-phase observations and timings:
[rectangles](evidence/r01/residency-rect.log),
[paths](evidence/r01/residency-path.log),
[second rectangle process](evidence/r01/residency-rect-os-peak.log),
[second path process](evidence/r01/residency-path-os-peak.log).
Their maximum observed `/proc` VmHWM values are 4,080 / 10,184 KiB in the first
processes and 4,104 / 10,276 KiB in the second processes. These phase reads are
OS observations, not an exact continuously sampled allocator high-water mark.
The compiler-excluded `getrusage` child peak is 10,112 KiB for both second runs;
the [same-launcher /bin/true control](evidence/r01/rss-launch-control.log) is
also 10,112 KiB. Thus that OS metric includes the fork/exec launch floor and
cannot distinguish these small probe peaks by itself. All three measurements
are retained separately; no pool reservation or command count is presented as
actual source/private allocation or pure test-process RSS.

These VM observations establish neither a hard elapsed-time deadline nor
low-end/device performance. In particular, complete path export spans multiple
slices and exceeds 1ms. Upstream record, atomic capacity allocation, finish,
cancellation and source/cache destruction remain separately scheduled costs.
Opaque source Scene/cache capacities and unreferenced private definitions do
not have a command-derived hard quota. A future hard producer limit or sliced
destructor needs a real upstream owned-parts/drain/storage interface, as already
specified in §16; this task adds no imaging source fork.

## Reproduction and verification scope

Use the pinned Rust 1.96 toolchain and Linux Fontconfig/pkg-config environment
(see environment.log). No installed toolchain/font/native shim is committed.

```sh
cargo fmt --all --check
cargo fmt --manifest-path third_party/masonry/Cargo.toml --all --check
cargo test --workspace --features reel-core/test-support --locked -- --test-threads=1 --nocapture
cargo test --workspace --release --features reel-core/test-support --locked -- --test-threads=1 --nocapture
cargo clippy --workspace --all-targets --features reel-core/test-support --locked -- -D warnings
CARGO_TARGET_DIR="$PWD/target" cargo clippy --manifest-path third_party/masonry/Cargo.toml -p masonry_core -p tree_arena --all-targets -- -D warnings
CARGO_TARGET_DIR="$PWD/target" cargo test --manifest-path third_party/masonry/Cargo.toml -p masonry_core -p tree_arena --lib -- --test-threads=1
CARGO_TARGET_DIR="$PWD/target" cargo test --manifest-path third_party/masonry/Cargo.toml -p masonry_core -p tree_arena --lib --release -- --test-threads=1
REEL_R01_TEST_FONT=/path/to/font.ttf cargo test -p reel-stage --test r01 actual_parley_label_glyphs_are_copied_without_reshaping --locked -- --ignored --test-threads=1 --nocapture
REEL_R01_TEST_FONT=/path/to/font.ttf cargo test -p reel-stage --release --test r01 actual_parley_label_glyphs_are_copied_without_reshaping --locked -- --ignored --test-threads=1 --nocapture
cargo test -p reel-stage --release --test r01_measurements --locked --no-run --message-format=json > /tmp/reel-r01-build.jsonl
python3 crates/reel-stage/scripts/measure-r01.py /tmp/reel-r01-build.jsonl /tmp/r01-rect.log --case rect
python3 crates/reel-stage/scripts/measure-r01.py /tmp/reel-r01-build.jsonl /tmp/r01-path.log --case path
python3 crates/reel-stage/scripts/measure-r01.py /tmp/reel-r01-build.jsonl /tmp/r01-launch.log --case rect --launch-control
cargo tree -p reel-render --edges normal --locked
cargo tree -p reel-render --edges features --locked
python3 crates/reel-stage/scripts/audit-source.py /path/to/extracted-xilem-271a27a6
```

Formatting and workspace/direct selected-core strict Clippy pass. Normal and
feature trees are saved in [normal](evidence/r01/normal-dependencies.log) and
[features](evidence/r01/feature-dependencies.log). The normal tree has one local
masonry_core/tree_arena, core, stage and pinned ordinary imaging; no winit/wgpu,
player, VM, imaging_vello or second UI source. Core reads neither stage nor
render; Masonry reads no Reel crate. The two creator guides state Rust-only
implementation status and retain identical Aside snippets. The latest full
architecture remains intact, with R01 implementation/evidence links added.

Recording/null consumers and analytical color checks are the verified scope.
No GPU rasterization/compositing, window/player, device loss recovery, native
target or actual presentation performance is claimed. Existing U02 historical
CPU/input/geometry evidence is preserved. No blocked V03 independent VM execution
test, main update, Actions enablement, ready/merge or acceptance comment is part
of this task. There is no unresolved R01 implementation blocker within §16's
agreed boundaries; device rasterization and window integration remain next tasks.
