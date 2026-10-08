# L01 storage validation

Contract: Issue #4, design 1.1 / `5a7cfd4`. Interface draft was submitted first
as `c387a73`; implementation is `398a740`, load harness revision is `c1fecab`.
D01 commit `ea2375e3c088ae4cbd9af3b5a50c64b63338d488` is merged as the shared
workspace base. Review this task against that commit to exclude D01's changes.
This task only adds `crates/aside-vm/**` and generated dependency lock entries;
root Cargo.toml/toolchain and reel-core come unchanged from D01.

## Commands and results

Executed on 2026-10-08, Rust 1.90.0 / LLVM 20.1.8, Linux x64 KVM container,
AMD EPYC 9V45 with five exposed CPUs. Toolchain was absent initially and installed
under `/workspace/aside-toolchain`, outside the repository. No repository-local
skill directory was present. No GitHub Actions were added.

Environment used for every Cargo invocation:

```sh
export RUSTUP_HOME=/workspace/aside-toolchain/rustup
export CARGO_HOME=/workspace/aside-toolchain/cargo
export CARGO_TARGET_DIR=/workspace/aside-target
export PATH="$CARGO_HOME/bin:$PATH"
cd /workspace/reel
```

| Command | Actual result |
| --- | --- |
| `cargo fmt --all -- --check` | Exit 0, formatted |
| `cargo clippy -p aside-vm --all-targets -- -D warnings` | Exit 0, no warnings |
| `cargo test -p aside-vm` | Exit 0, all storage behavior checks passed |
| `cargo test -p aside-vm --release` | Exit 0, same behavior checks passed with optimizations |
| `rustup target add i686-unknown-linux-gnu` | Exit 0, cross std installed |
| `cargo check -p aside-vm --all-targets --target i686-unknown-linux-gnu` | Exit 0, library, tests and example type-checked; layout assertions evaluated |
| `cargo build -p aside-vm --lib --target i686-unknown-linux-gnu` | Exit 0, 32-bit rlib compiled; no executable/device run |
| `cargo tree -p aside-vm -e normal,build` | Only slotmap 1.1.1 and its build dependency version_check 0.9.5; no Reel/platform/gc-arena dependency |
| `cargo run -p aside-vm --release --example storage_load` | Exit 0; seven fixed loads, zero GC allocations, all objects/payload reclaimed |
| `/workspace/aside-target/release/examples/storage_load` (two further sequential runs) | Both exit 0; raw outputs retained |
| `git diff --check` | Exit 0 |

Production library and example inherit D01's `unsafe_code = forbid`. No unsafe
code, imported upstream source or script/native destructor is added. The pinned
allocation-counter 0.8.1 dev dependency instruments the allocator through its
safe API; it is absent from production dependencies.

## Behavioral evidence

Runnable checks are in [tests/storage.rs](tests/storage.rs), with adapter/identity
checks alongside their source modules. They exercise actual stored graphs:

- i64 minimum/maximum and values beyond exact f64 integers; every tested float
  bit pattern (NaN payloads, infinities, subnormal, negative zero) round-trips.
  100,000 Value copies allocate nothing and do not retain an unrooted object.
- Complete high-generation u64 KeyData values round-trip without narrowing;
  canonical-key checks reject normalized even generations. Foreign heap/epoch
  handles, roots and strong edges are rejected. A fresh restore heap rejects old
  handles. HeapId allocation exhaustion never wraps or reuses its u32 identity.
- The same deletion adapter uses a reduced occupied-version threshold of 3;
  object and root slots retire after two lifecycles. Every old handle/token stays
  invalid, retired slots stay charged and eventually enforce capacity limits.
- Two closure records share one real mutable cell, and tuple elements retain
  aliases. Tuple -> closure -> cell -> array -> tuple cycles reclaim after root
  removal. No tuple write or identity weak target is exposed.
- After each one-unit GC call, a 200-cell array is reordered, popped/appended,
  and an already registered root is changed. Every currently reachable cell
  survives. Updates behind the root/container scan cursors use the same barrier.
- New objects' initial edges and weak upgrades during marking shade their
  targets. At the Weak cutoff, generation-valid white targets already refuse
  upgrade, root publication, strong-edge writes and new-object initial fields.
  The refusal continues through the final Sweep step. Confirmed-live and newly
  allocated targets continue to upgrade. Weak fields never retain their targets.
- Allocations interleaved with Weak and Sweep do not skip or duplicate old sweep
  records. Post-cutoff objects survive that cycle and disappear in the next if
  unrooted. Root owner release proceeds by a persistent, bounded slot cursor.
- A 4,096-element array and large UTF-8 buffer release by independent edge/byte
  quotas. Objects reject access before their payload is freed; pending bytes
  remain charged. An insufficient byte quota reports blocked while edge teardown
  can progress. Allocation remains denied until maintenance returns capacity.
- A 10,000-cell chain and cycles free without recursive graph destruction or GC
  allocation. Removing the last element from 128 still-live arrays returns all
  payload allocator bytes, including empty BTreeMap nodes, immediately.

## Fixed loads

[LOAD-RESULTS.txt](LOAD-RESULTS.txt) contains all three final sequential runs,
including per-phase maxima, p50/p95/p99, allocation totals, peaks, queue/pending
accounting, edge/byte releases and final object counts. Each load first collects
with an explicit host root, verifies survival, removes that root, then collects
again. Values and heap handles held by the harness are deliberately not roots.

Time is measured around actual API/GC calls using Instant. The allocator remains
instrumented, so allocation/free hooks contribute to these timings. The phase
quotas stop each measured call before work in the next phase. Normal slices use
256 total units and 16 KiB of charged release bytes. The final buffer load uses
one unit, isolating each chunk free. Mutation maxima include heap construction,
object construction, and bounded writes/appends. Heap drop is measured separately
after GC has drained payload; it is synchronous and not a scheduled GC operation.

The following maxima span the three final runs; memory/allocation totals were
identical between runs. Times are microseconds and byte counts are exact.

| Fixed load | Max GC call µs | Max storage call µs | Allocations | Allocator peak bytes | Charged peak bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| 10,000 shared-cell updates | 4.116 | 2.494 | 12 | 15,960 | 20,008 |
| 10,000-cell deep chain | 13.640 | 33.480 | 6 | 1,731,240 | 1,828,136 |
| 2,000 two-array cycles | 4.126 | 39.910 | 8,043 | 5,428,824 | 9,021,672 |
| 100,000-element array | 33.781 | 22.814 | 1,829 | 1,645,896 | 3,206,376 |
| 1 MiB combining/ZWJ/NUL UTF-8 buffer | 13.110 | 20.220 | 1,201 | 1,079,992 | 2,102,504 |
| 1 MiB binary buffer, 4 KiB appends | 0.521 | 10.726 | 1,201 | 1,079,992 | 2,102,504 |
| Same binary buffer, one-unit GC | 0.210 | 19.138 | 1,201 | 1,079,992 | 2,102,504 |

Every load observed zero allocations in GC calls, zero objects/pending payload
at completion and zero net allocator bytes after dropping Heap. A one-unit sweep
call, including an individual chunk free, was at most 0.210 µs. Largest final-run
Heap drop was 25.849 µs. Cycle release backlog peaked at 7,890,944 charged bytes;
this is included in the byte ceiling, with all reserved queues counted separately.
The allocator numbers measure requested heap bytes, not RSS or allocator-internal
pages. Charged bytes deliberately overestimate tree/slot metadata; the harness
asserts its allocator peak is covered by charged peak.

An earlier exploratory run of the same buffer API, before the measurement-only
harness revision, observed a 420.973 µs storage-call outlier. Final-run maxima
are observations, not a wall-clock guarantee; preemption and allocator tails
must remain part of device measurements. This storage load does not establish
full VM, N100 or Vita performance.

## Platform and implementation limits

x64 behavior ran in this container: Value size 16, alignment 8. i686 is a compile
check: the library rlib compiles, and static assertions confirm size 16 and
alignment 4. A standalone check linked against that rlib also compiled with
explicit `size_of::<aside_vm::Value>() == 16` and
`align_of::<aside_vm::Value>() == 4` assertions. No 32-bit
binary was linked/run and no physical device or Vita runner/SDK was available.
Numeric/GC behavior on a real 32-bit device remains unverified; i686 compilation
must not be described as Vita/device execution.

This is the Issue #4 storage milestone. Parser, bytecode execution, scheduling,
full collection/text semantics, native adapters and snapshot/restore codecs are
not implemented. Restore invalidation is demonstrated with a fresh heap identity.
General operations will need continuations or input caps at their own boundary.
The integration owner controls D01/this PR merge order and maintains author guides.

Standalone i686 layout check (also exited 0, no executable run):

```sh
cat > /tmp/aside-layout32.rs <<'RS'
extern crate aside_vm;
const _: () = assert!(std::mem::size_of::<aside_vm::Value>() == 16);
const _: () = assert!(std::mem::align_of::<aside_vm::Value>() == 4);
RS
rustc --target i686-unknown-linux-gnu --crate-type lib --edition 2024 \
  -L dependency=/workspace/aside-target/i686-unknown-linux-gnu/debug/deps \
  --extern aside_vm=/workspace/aside-target/i686-unknown-linux-gnu/debug/libaside_vm.rlib \
  /tmp/aside-layout32.rs -o /tmp/libaside_layout32.rlib
```

## Focused swap-barrier check (review follow-up)

`array_swap_alone_preserves_an_unscanned_child_moved_behind_the_cursor` roots
only `[null, child]`. After root scanning, one edge unit visits index 0; the
sole child remains white at unscanned index 1. A single swap moves it to index
0 behind the cursor. No root changes, weak upgrades or pop/push operations occur.
After collection, the array still points to the child and its stored integer 42
remains readable.

The negative control temporarily removed only the two `self.barrier(...)`
calls inside `array_swap`. Both profiles then failed at the final child-read
assertion with `unscanned child moved behind the cursor must survive:
InvalidHandle`. The production method was restored byte-for-byte before the
positive checks below and was not changed in the submitted commit.

| Command (same environment as above) | Barriers disabled | Production restored |
| --- | --- | --- |
| `cargo test -p aside-vm --test storage array_swap_alone_preserves_an_unscanned_child_moved_behind_the_cursor -- --exact` | Exit 101, expected child-survival assertion failure | Exit 0 |
| `cargo test -p aside-vm --release --test storage array_swap_alone_preserves_an_unscanned_child_moved_behind_the_cursor -- --exact` | Exit 101, same assertion failure | Exit 0 |
| `cargo fmt --all -- --check` | — | Exit 0 |
| `cargo clippy -p aside-vm --all-targets -- -D warnings` | — | Exit 0 |

The PhantomData comment now states that Values remain in the heap's
single-threaded execution domain while HeapId allocation is process-global.
No fixed load or timing run was repeated for this focused verification change.
