# R01 result — portable frames and render interfaces

Historical observations below describe their original revisions. Raw evidence links
point to the pre-cleanup Git revision; new generated output belongs under
`target/reel-artifacts/`. Current maintenance commands are documented in
[`crates/reel-dev`](../../crates/reel-dev/README.md). Issue #34 migration and current validation are recorded in [MIGRATION.md](MIGRATION.md).

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

## Work-budget diagnostic ownership repair (after `411cc753`)

The owning WidgetId contract applies to `Work` errors too. A public-API regression
reproduced the remaining boundary with only two actual Masonry Draw commands and
no clip: allowance 20 and slice allowance 1 reported child command 1 with its
parent's owner. [Before-fix output](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-work-owner/before-fix.log) retains
that failure; this agrees with the owner's independent debug/release counterexample.

Each compiler cursor now retains a monotonically advancing owner-range index.
Setup seeds the first range using one indexed read. Main command commit and Scene
fragment advance prepay three units, including the constant range/owner update;
External fragment advance is included in its prepaid header work. Each command
crosses at most one contiguous range, without a scan or binary search. The next
command's owner is therefore already current before mapping prepayment can fail.
Mask cursors retain their defining owner; paused payload/allocation work retains
the selected main owner. Error reporting itself only reads the stored cursor.
Per-command range validation is still prepaid. Minimum indivisible advance costs
are reported through `minimum_units`; the work ledger is deliberately updated to
cover the additional cursor work, so old numerical failure cutoffs may move.

The regression sweeps total allowances 0..95 with slice allowances 1 and 64,
covering adjacent parent/child draws and the first draw belonging to an Inline
child of a nondrawing fragment owner: 384 public builds per profile. Every actual
command's Work owner is checked against its painter, aggregate/per-slice units
remain within allowance, and failure/finished-frame drop releases output and
candidate eligibility. Each configuration reaches 21 child-owned Work failures
and successful publication. The final one-past-stream probe is an end marker,
not an actual command. [Debug](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-work-owner/debug-tests.log) and
[release](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-work-owner/release-tests.log) pass 14 R01 cases, the original
24 U02 contracts, zero-allocation dispatch, 12 core and six render contracts.
[Strict Clippy](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-work-owner/clippy.log), fmt and
[recorded commands](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-work-owner/checks.log) pass. Real Parley retains
one run / 14 positioned glyphs in [debug](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-work-owner/parley-debug.log)
and [release](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-work-owner/parley-release.log). No Masonry production
source/dependency changes were needed; its [source audit](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-work-owner/source-audit.log)
remains 94 fixed files, one existing local source and 212,652 diff bytes.

Two isolated processes per original workload, each with 20 warm samples, remeasure
complete export including setup, all steps and finish/source cleanup:

| Observed wall time after owner-cursor repair | Rectangles | Large paths |
| --- | ---: | ---: |
| Warm complete build p50 | 0.368–0.375 ms | 2.471–2.582 ms |
| Warm complete build p95 | 0.469–0.494 ms | 2.675–2.739 ms |
| Cold complete export | 0.490–0.492 ms | 3.033–3.088 ms |
| Maximum cold step | 5.583–24.876 µs | 38.793–45.181 µs |
| Finish/source cleanup | 16.023–16.486 µs | 3.120–3.310 µs |
| Paused cancellation | 2.569–2.873 µs | 2.217–2.393 µs |
| Isolated source Drop | 2.053–2.458 µs | 2.189–2.249 µs |

The range index adds eight bytes per bounded Cursor: the configured 32-cursor
scratch reservation and actual setup allocation increase by 256 bytes. Full
export still uses 11 / 27 allocation calls and zero reallocations, now requesting
303,896 / 1,858,072 bytes with 258 / 901 steps. Frame accounting is
305,056 / 1,859,232 against its continuous 16MiB ticket. Measured record residency
and source Drop are unchanged; live requested-byte phase peaks are now
1,142,232 / 7,414,488, with the extra scratch freed at finish. Immutable-frame
residency remains 856,328 / 5,558,536; source Drop still frees
277,192 / 1,847,240 bytes in eight / 24 deallocations. These source/private,
physical allocator observations remain distinct from output reservations.
[Rectangles 1](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-work-owner/residency-rect-1.log),
[rectangles 2](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-work-owner/residency-rect-2.log),
[paths 1](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-work-owner/residency-path-1.log),
[paths 2](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-work-owner/residency-path-2.log) and the
[RSS launch control](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-work-owner/rss-launch-control.log) retain the
full observations. This does not establish a wall-clock deadline or GPU behavior.
Previous repair evidence below remains unchanged. The same draft branch is used;
owner independent revalidation remains pending, with no execution blocker.

## Initial review repairs (`411cc753`, 2026-10-09)

All five Issue #25 findings were reproduced against `73589e9` with actual
Masonry records and the public consumers: every new check failed as expected.
[Before-fix output](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/before-fixes.log) records the admitted
second 4-byte upload in a 4-byte queue, `Bytes` on an expired one-descriptor slot,
Inline child ownership assigned to its ancestor, double-transformed debug bounds,
and loss of eligibility after the last frame's render completion.

The repairs are confined to stage/render and the existing licensed Masonry paint
closure. Aggregate queued payload-capacity tickets now have their own upload
limit, independent of retained residency; rejection preserves the original batch
and tickets. Completed, failed and cancelled uploads restore queue capacity.
Slot reuse releases the registry's expired descriptor before replacement
reservation; external weak aliases still keep their own ticket and can correctly
prevent replacement until they drop. Inline paint command ranges preserve actual
owners for normal draws, invalid strokes/transforms and missing images, including
paused payload work. Debug bounds use window-space identity fragments, with
forward baselines and continuous scopes; translated/scaled and finite singular
roots with child scrolling match the same frozen geometry.

`PresentationNotice::new` replaces struct literals, with read-only accessors.
An Accepted notice independently owns qualification and matching geometry until
resolve or final notice drop, without keeping frame commands/resources. The
public consumer regression submits, creates/clones a notice, drops the caller
frame, completes rendering (output pool usage is zero), then accepts the exact
geometry. Last unresolved notice drop cancels; epoch changes remain stale.
Ordinary keys do not extend qualification or resurrect cancellation. Build
constructor/step/finish failure and cancel/drop explicitly invalidate qualification,
even with an outstanding notice. U02 identity, ordering, first-frame, capture,
clear and epoch gates remain intact. No upload/build completion accepts input.

At `411cc753`, debug and release runs pass 12 core, 24 unchanged U02 behavioral contracts,
the zero-allocation pointer-dispatch contract, 13 R01 cases and six render cases.
Real Parley independently passes both profiles with one run / 14 original glyphs.
Workspace and selected masonry_core/tree_arena strict all-target Clippy, both fmt
checks and direct core debug/release checks pass. [Debug](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/debug-tests.log),
[release](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/release-tests.log), [final commands](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/final-checks.log),
[Clippy](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/clippy.log), [Masonry Clippy](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/masonry-clippy.log),
[Parley debug](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/parley-debug.log),
[Parley release](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/parley-release.log),
[Masonry debug](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/masonry-debug.log) and
[Masonry release](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/masonry-release.log) retain the outputs.
The [source audit](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/source-audit.log) verifies the same 94 fixed
upstream files and one existing local source, with a regenerated 212,652-byte
production diff. [Normal](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/normal-dependencies.log) and
[feature](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/feature-dependencies.log) trees retain the same
single-source dependency boundaries. Both guide editions and the latest full
architecture were updated together; their Aside snippets remain identical.

The `411cc753` isolated release probes use the original workloads, slicing, allocator
instrumentation and 20 warm samples per process, two processes per workload.
Full export includes setup, every step, finish and source cleanup:

| Observed wall time at `411cc753` | 512 rectangles | 16 × (move + 2,048 lines) |
| --- | ---: | ---: |
| Cold real record | 0.356–0.499 ms | 1.418–2.352 ms |
| Cold complete export | 0.521–0.604 ms | 3.305–3.698 ms |
| Warm complete build p50 | 0.374–0.376 ms | 2.539–2.687 ms |
| Warm complete build p95 | 0.423–0.429 ms | 2.644–2.804 ms |
| Warm complete build maximum | 0.424–0.445 ms | 2.762–2.963 ms |
| Maximum step in cold probe | 33.434–43.703 µs | 72.361–148.654 µs |
| Finish/source cleanup | 18.239–24.022 µs | 3.180–3.924 µs |
| Paused cancellation | 2.570–2.759 µs | 2.314–2.584 µs |
| Isolated source Drop, cache retained | 2.538–2.553 µs | 2.011–2.165 µs |
| Producer/cache Drop | 40.338–48.662 µs | 8.935–11.978 µs |

Complete export still allocates 11 / 27 times, requests 303,640 / 1,857,816 bytes
and performs zero reallocations, using 258 / 901 steps. Record allocates 15 / 47
times and requests 552,308 / 3,696,372 bytes (+312 versus the initial baseline).
Measured live record/paused/complete residency increases by 216 bytes; immutable
frame and cache-retained phases increase by 24 bytes. Actual live requested-byte
peaks in these phase observations are 1,141,976 / 7,414,232; after source release
856,328 / 5,558,536; after cancellation 561,440 / 3,709,472; after cache Drop 712.
Source Drop releases 277,192 / 1,847,240 bytes in eight / 24 deallocations.
Frame accounting remains 304,800 / 1,858,976 against its continuous 16MiB ticket.

[Rectangle process 1](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/residency-rect-1.log),
[process 2](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/residency-rect-2.log),
[path process 1](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/residency-path-1.log) and
[process 2](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/residency-path-2.log) contain full allocator,
record→paused→finish/cancel and cleanup observations. Maximum reported phase
VmHWM is 4,040–4,048 / 10,232–10,240 KiB. Child `getrusage` peaks are
9,856–9,984 KiB and the [launch control](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/rss-launch-control.log)
is 9,984 KiB, preserving the launch-floor limitation. These remain phase/OS
observations, not an exact continuous allocator peak or a hard source quota.
A preliminary probe before the last cancellation-path repair had wider timing
variation: rectangle p95 0.864–1.215ms, path p95 2.706–3.074ms, maximum steps
209.727 / 261.855µs. The intermediate pass before owner-lookup deduplication is also retained in
[rectangle](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/intermediate-residency-rect-1.log),
[second rectangle](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/intermediate-residency-rect-2.log),
[path](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/intermediate-residency-path-1.log) and
[second path](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/intermediate-residency-path-2.log). The first pass's [printed timing summary](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/pre-final-probe-summary.log)
is retained separately; it is not the final binary and no latency guarantee or
cause of variation is inferred. No device-performance claim is made.

Use the existing reproduction commands below, limiting the test command to
`-p reel-core -p reel-ui -p reel-render`. The measurement command's final
compiler artifact is saved in [measurement-build.jsonl](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/measurement-build.jsonl).
The resumed workspace remained executable after the reported disconnect;
[environment.log](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01-review/environment.log) preserves the exact initial
Rust read-only-home error. Using the already installed workspace toolchain's
CARGO_HOME/RUSTUP_HOME resolved it, with no escalation or system-directory write.
There is no remaining execution blocker. This stays a draft for owner revalidation;
GPU rasterization/compositing, windows/player and device acceptance remain outside
R01. No independent VM execution test, main update, merge/ready transition,
Actions enablement or acceptance comment was performed.

## Initial behavior evidence (`73589e9`)

The full debug/release workspace runs pass 12 core contracts, 24 original U02
contracts, the real pointer allocation contract, ten R01 portable-frame cases
and four render-consumer cases. The separately invoked real Parley Label case
passes in both profiles: one run, 14 positioned glyphs, unchanged glyph IDs,
positions, font index, size and variation coordinates. See
[debug](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01/debug-tests.log),
[release](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01/release-tests.log),
[final render debug](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01/render-debug-tests.log),
[Parley debug](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01/parley-debug.log) and
[Parley release](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01/parley-release.log).

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
[Source audit](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01/source-audit.log) verifies 94 fixed upstream files,
one existing local geometry source and the exact 206,315-byte production diff.
Original hashes, copyright headers and Apache-2.0 license are retained.

## Initial export and source residency observations (`73589e9`)

2026-10-08, Rust 1.96.0, Linux x86_64 KVM, Xeon Platinum 8573C, five exposed CPUs;
details and native Fontconfig setup are in [environment.log](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01/environment.log).
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
[rectangles](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01/residency-rect.log),
[paths](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01/residency-path.log),
[second rectangle process](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01/residency-rect-os-peak.log),
[second path process](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01/residency-path-os-peak.log).
Their maximum observed `/proc` VmHWM values are 4,080 / 10,184 KiB in the first
processes and 4,104 / 10,276 KiB in the second processes. These phase reads are
OS observations, not an exact continuously sampled allocator high-water mark.
The compiler-excluded `getrusage` child peak is 10,112 KiB for both second runs;
the [same-launcher /bin/true control](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01/rss-launch-control.log) is
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

## Current reproduction and verification scope

Use the pinned Rust 1.96 toolchain and Linux Fontconfig/pkg-config environment
(see environment.log). No installed toolchain/font/native shim is committed.

```sh
cargo fmt --all --check
cargo fmt --manifest-path Cargo.toml --all --check
cargo test -p reel-ui -p reel-render -p reel-player -p reel-core --features reel-core/test-support --locked -- --test-threads=1 --nocapture
cargo test -p reel-ui -p reel-render -p reel-player -p reel-core --release --features reel-core/test-support --locked -- --test-threads=1 --nocapture
cargo clippy -p reel-ui -p reel-render -p reel-player -p reel-core --all-targets --features reel-core/test-support --locked -- -D warnings
CARGO_TARGET_DIR="$PWD/target" cargo clippy --manifest-path Cargo.toml -p reel-ui --all-targets -- -D warnings
CARGO_TARGET_DIR="$PWD/target" cargo test --manifest-path Cargo.toml -p reel-ui --lib -- --test-threads=1
CARGO_TARGET_DIR="$PWD/target" cargo test --manifest-path Cargo.toml -p reel-ui --lib --release -- --test-threads=1
REEL_R01_TEST_FONT=/path/to/font.ttf cargo test -p reel-ui --test r01 actual_parley_label_glyphs_are_copied_without_reshaping --locked -- --ignored --test-threads=1 --nocapture
REEL_R01_TEST_FONT=/path/to/font.ttf cargo test -p reel-ui --release --test r01 actual_parley_label_glyphs_are_copied_without_reshaping --locked -- --ignored --test-threads=1 --nocapture
mkdir -p target/reel-artifacts
cargo test -p reel-ui --release --test r01_measurements --locked --no-run --message-format=json > target/reel-artifacts/r01-build.jsonl
REEL_R01_PROBE_CASE=rect cargo run --locked -p reel-dev -- measure target/reel-artifacts/r01-build.jsonl r01_measurements --ignored
REEL_R01_PROBE_CASE=path cargo run --locked -p reel-dev -- measure target/reel-artifacts/r01-build.jsonl r01_measurements --ignored
cargo run --locked -p reel-dev -- measure target/reel-artifacts/r01-build.jsonl r01_measurements --launch-control
cargo tree -p reel-render --edges normal --locked
cargo tree -p reel-render --edges features --locked
cargo run --locked -p reel-dev -- audit-ui
```

Formatting and workspace/direct selected-core strict Clippy pass. Normal and
feature trees are saved in [normal](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01/normal-dependencies.log) and
[features](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/reel-stage/evidence/r01/feature-dependencies.log). The normal tree has one local
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
