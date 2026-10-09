# V02 implementation and validation

Issue #17 / draft PR https://github.com/jhq223/reel/pull/18.
The owner approved the b68045d interface/ownership draft before implementation.
Frozen semantics: section 22 at 1226c6b39dfdbd13eca665accd2ecd95bcb7d5d2,
read via git show, without importing frontend code. Accepted D01 c03226a and
VM 95a7e84 are combined in local baseline 04784f1. The task's changes can be
reviewed with `git diff 04784f1..HEAD -- crates/aside-vm`.

Only range/window/heap/gc/slot adaptation, necessary exports, tests, focused
load/probe support and storage documentation changed. No bytecode directory,
opcode, dispatcher, crate or dependency was added. Author-visible semantics
remain the owner's frozen contract; no creator-guide behavior changed.

## Implemented behavior and ownership

- FrameSlots owns one Value Vec, a separate u64 initialization bitmap, a fixed
  generation-checked range mapping Vec and a bounded sorted free-span Vec. The
  mapping is per range. Reads/writes index a base+u32 position and a single bit
  in constant work; they never scan other registers. Whole-window validation is
  explicit in `borrow_initialized`, not hidden in register access.
- RootRangeToken includes HeapId/SessionEpoch/full RangeKey. Heap keeps only
  owner/kind/fixed-length descriptors, scan indices/cursors and accounting
  metadata. GC resolves a temporary RootRanges view; it saves no slice/address.
  All null contents and cleared bitmap bits precede atomic descriptor/mapping
  publication with exclusive borrows and no callbacks/GC in between.
- All real publishing writes use the existing insertion barrier. Moves establish
  the destination before clearing the source. Host transfer creates its RootToken
  first; a failed transfer preserves the source. Reading a bare delivery Value
  keeps the delivery registration. Abandonment explicitly releases the range.
- One collector powers both GC entries. The legacy entry returns MissingRootProvider
  with any live descriptor before work or phase changes, including Idle, Weak,
  Sweep and zero-unit calls. Descriptors, holes and every Value (including null)
  independently consume roots/units. Full-generation changes restart descriptor
  work and offset zero; never-used descriptor capacity adds no scan work.
- Single roots, all ranges and gray work finish before the original unified weak
  cutoff. Post-cutoff white-object publication still returns UnconfirmedLive.
  Object invalidation, pending payload release and slot retirement are preserved.
- Normal close preflights all mappings/descriptors before unregistering/freeing.
  Wrong heap, missing mapping and length mismatch leave ownership/charges intact.
  Successful close allows reattachment; an old idempotent close cannot detach a
  replacement. Cancellation cursors bind heap/epoch/attachment/owner. The owning
  executor must prohibit new roots for an owner while cancellation advances.
- Unexpected pool drop leaves descriptors and makes GC/reattachment explicitly
  fail. Empty-pool drop permits attachment. Heap drop invalidates surviving pool
  access, and frees its own physical storage before refunding that reservation.
- Shared byte accounting includes window capacity, bitmap, mappings, free spans,
  descriptor indices/retired slots and inline control metadata. ChargedVec field
  order frees data before its ticket. Physical relocation holds old/new buffers
  and bitmaps together, keeps identity/length/order/bits and publishes new bases
  only after all copying/barriers succeed. Failure keeps old storage usable.
  Pool holes never refund retained capacity. Close frees buffers/metadata Vecs;
  its still-existing inline controller remains charged until that owner drops.
  A live observer keeps the physical 64-byte native Rc byte book charged too.

## Actual commands and results

Executed 2026-10-08 in the available Linux x86_64 KVM container, five exposed
CPUs, Intel Xeon Platinum 8573C. Rust 1.96.0 ac68faa20 / pinned slotmap 1.1.1.
Official rustup/toolchain/components live outside the repo. A lightweight pwd,
git status/rev-parse and rustc command confirmed the environment remained usable
after the disconnection notice; successful builds/tests continued afterward.

Every Cargo/rustc command used:

```sh
export RUSTUP_HOME=/workspace/issue17-toolchain/rustup
export CARGO_HOME=/workspace/issue17-toolchain/cargo
export CARGO_TARGET_DIR=/workspace/issue17-target
export PATH="$CARGO_HOME/bin:$PATH"
```

| Actual command | Result |
| --- | --- |
| `cargo test -p aside-vm --locked` | Passed original heap graphs, retirement, budget/pending release and new window behaviors |
| `cargo fmt --all -- --check` | Exit 0 |
| `rustfmt --edition 2024 --check crates/aside-vm/tests/support/root_range_dealloc_probe.rs` | Exit 0 |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | Exit 0, no warnings |
| `cargo test --workspace --all-features --locked` | Exit 0, original heap, V02, reel-core contracts and borrow compile-fail check |
| `cargo test --workspace --all-features --release --locked` | Exit 0, same behavior with optimization |
| `cargo check --workspace --all-targets --all-features --locked --target i686-unknown-linux-gnu` | Exit 0, library/tests/examples cross-checked |
| `cargo build -p aside-vm --lib --locked --target i686-unknown-linux-gnu` | Exit 0, 32-bit rlib compiled |
| `cargo tree -p aside-vm -e normal,build --locked` | Only slotmap 1.1.1 and version_check 0.9.5; allocation-counter remains the existing dev dependency |
| `cargo run -p aside-vm --release --locked --example storage_load` | All original seven graph/buffer loads passed, GC allocated nothing and all final payload/objects reclaimed |
| `cargo run -p aside-vm --release --locked --example frame_slots_load` | Numeric API/initialization and window lifecycle costs measured; all allocator/charge and stored-result checks passed |
| `bash crates/aside-vm/tests/support/run_root_range_dealloc_probe.sh` | Debug/release actual System-dealloc observations passed |
| `cargo run --locked -p reel-dev -- root-negative` | Nine expected failures in a temporary source copy; production source digests unchanged |
| `git diff --check` | Exit 0 |

Native behavior tests interleave writes, swaps, moves, push/pop, partial-scan
reuse, close/reattach, physical relocation and result/owner transfer with every
one-unit GC call. They read real child contents/aliases and collect again after
release. Isolated white-child cases avoid weak upgrades or another mutation
accidentally saving the target. Range-generation retirement uses the production
adapter at threshold three and verifies every stale token plus capacity/charge.
Internal fault injection checks missing mapping/length and error-path ownership.
Pool drop and missing legacy provider are covered in all four collector phases.
The compile-fail check proves a borrowed argument view cannot cross relocation.

## Actual allocator-boundary and negative-control evidence

The standalone observer is outside Cargo's production crate/lints. Its only
unsafe code forwards caller-provided GlobalAlloc pointers/layouts to System
exactly once; it uses integer addresses, a read-only live budget view and fixed
TLS record arrays. Hooks do not dereference, format, allocate or panic. The library
still has unsafe_code=forbid. No production unsafe code or new crate was needed.

Before System.dealloc, each observation compares total charged bytes with all
tracked still-physical allocations. Five cases observe 5/12/7/8/4 actual frees:
partial relocation failure, Heap teardown, rejected constructor and reattachment,
relocation/reuse/shrink/close, and unexpected pool drop. At full overlap, the
first 1,024-byte old-buffer dealloc observed 3,312 tracked bytes still live,
5,968 bytes charged, and zero remaining quota. Failed bitmap reservation likewise
observed the 1,024-byte scratch buffer still charged at zero remaining quota.
The old registered window then remained usable. Cancellation/reuse caused no
physical frees or refunds. Heap teardown held its charge through every buffer
free, invalidated pool access, and left only the observer's physical 64-byte book.
Raw logs are retained in [ROOT-RANGES-RESULTS.txt](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/aside-vm/ROOT-RANGES-RESULTS.txt).

The independent controls removed just the write, swap or move barrier in isolated
source copies: debug/release then failed when reading the real child with
InvalidHandle. Comparing cursor slot alone instead of full generation produced
an actual old-offset-five/new-length-two bounds failure in both profiles. Reversing
ChargedVec's drop field order made the System-dealloc observer fail with
"budget refunded while allocator still owns bytes". Production source was never
edited for these controls; the passing workspace checks ran afterward.

## Observed cost and implementation limits

Two sequential window measurement runs used allocator instrumentation throughout.
Each numeric case performs 131,072 real API iterations at 1/16/256/4096 registers,
in Idle and Mark. Checksums verify actual stored integers. All loops allocate
zero objects/blocks/single roots and keep exactly one descriptor. Whole Heap/pool
setup performs 13 allocations at every size; capacity bytes scale, allocation
count does not. Read+barrier-write iteration averages were 12.281–14.209 ns;
adding clear and the failing-initialization read averaged 21.596–26.451 ns.
Samples are batches of 256 iterations, not per-operation latency guarantees.
The largest observed numeric batch was 583.561 microseconds, retained in raw
results; this container's timing tails are not a device performance claim.

At 4,096 live Values, logical null/bitmap initialization took 6.052–7.242 us;
the measured 8,192-capacity pool construction took 37.727 us. Relocation to
16,384 capacity took 125.395–130.640 us, holding 396,288 buffer/bitmap bytes
at once. Allocator peak was 402,272 versus 403,432 charged. Close took
6.957–7.104 us. One-unit GC's largest observed call was 10.706 us across the
two runs; all 4,096 Value units, one descriptor and one sentinel hole were
reported independently, with zero GC allocations and zero net bytes after drop.

The original storage loads retain zero GC allocations/final objects/payload and
zero allocator bytes after Heap drop. Their measured peaks stayed under charge
on Rust 1.96. The new default 1,024-descriptor capacity plus byte book adds
32,864 actual allocator bytes and 41,184 charged bytes, with three fixed setup
allocations, to those original loads even before attaching a pool. No scan work
is added for never-published descriptors. Hosts can lower max_root_ranges,
including zero to disable registration. These fixed capacities remain charged.

Pinned Rust source was inspected: alloc/src/raw_vec/mod.rs grow_exact and
try_allocate_in, alloc/src/alloc.rs Global allocation/deallocation, and
alloc/src/rc.rs RcInner layout. Exact new Vec capacity is reserved before
allocation, checked against the adapter's expected capacity, and observed in
real allocator records. The slotmap retirement/layout adapter and the existing
BTreeMap payload reservation remain unchanged in semantics; original loads
rechecked their actual residency coverage with the new toolchain.

Open/relocation/close use explicit configured bounds and no script/native calls;
they are synchronous safe-point storage operations, not resumable executor work
or hard wall-clock deadlines. Free-span search/shifting costs up to configured
range capacity; register access does not. Future ScriptFrame/NativeFrame and
continuation allocations must use this same internal byte book when implemented.
This milestone allocates their root windows, not those executor structures.

i686 is compilation only: Value's 16-byte layout and native-u64 alignment
assertions evaluated for both ABIs. No 32-bit executable was linked/run; no
N100/Vita/device runner was available. Opcode/dispatcher/native callbacks,
complete VM execution and snapshot codecs are outside this PR. No environment
blocker remains. PR #18 stays draft for the owner's critical-code review and
independent rerun; nothing was merged or marked ready and no Actions were used.
