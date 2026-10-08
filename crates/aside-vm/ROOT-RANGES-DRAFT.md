# V02 root ranges and FrameSlots — interface draft for review

Status: the owner approved the b68045d interface draft on 2026-10-08;
V02 is now implemented and validated. This file records the reviewed contract.
Actual evidence and remaining limits are in [ROOT-RANGES-VALIDATION.md](ROOT-RANGES-VALIDATION.md).
Issue: https://github.com/jhq223/reel/issues/17.

The frozen contract is section 22 of `docs/aside-language.md` at
`1226c6b39dfdbd13eca665accd2ecd95bcb7d5d2`, read with `git show`.
The task branch combines accepted D01
`c03226a59a9dd4f8b448d4eea82262a1d6e57ef8` and accepted VM storage
`95a7e84fc49446937daedc3264d2d5970fa3af48` in local merge `04784f1`.
It does not import the frontend branch. Rust is pinned to 1.96.0.
`AGENTS.md` was read; repository and workspace `.agents/skills` are absent.

## Scope and consumers

Implement contiguous root windows, their initialization bitmap, generational
registration, provider borrowing, insertion barriers, GC cursor integration,
and memory accounting. Changes belong to aside-vm's range/window/heap/gc/slot
adapter, necessary lib exports, tests, focused load/probe support, and this
storage contract/validation documentation. No bytecode files, opcodes,
dispatcher, parser, scheduling, native callback protocol, or new crate.
The integration owner maintains the specification and creator guides; this
implements their frozen semantics without changing author-visible behavior.

All tasks, arguments, native temporary values, exception values and deliveries
can use the same pool with independent ranges and owners. A window is a storage
primitive; a ScriptFrame/NativeFrame and its continuation metadata belong to
the executor task. Those future allocations must use the same byte ceiling;
this task accounts for the window/provider metadata it actually allocates.
It does not claim that a working window constitutes a working executor.

## Reviewed public storage interfaces

The following Rust signatures are implemented exports.
Types have private representation unless fields are shown.

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RootRangeToken {
    // HeapId, SessionEpoch, complete generational RangeKey, !Send/!Sync marker.
}

pub struct FrameSlots { /* buffers, mappings, free spans, accounting lease */ }
pub struct RootRanges<'a> { /* immutable borrow of FrameSlots */ }

#[derive(Clone, Copy, Debug)]
pub struct FrameSlotsLimits {
    pub initial_capacity: u32,
    pub max_capacity: u32,
    pub max_window_len: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct RangePosition {
    pub range: RootRangeToken,
    pub index: u32,
}

impl FrameSlots {
    pub fn new(heap: &mut Heap, limits: FrameSlotsLimits)
        -> Result<Self, HeapError>;
    pub fn roots(&self) -> RootRanges<'_>;
    pub fn range_owner(&self, heap: &Heap, range: RootRangeToken)
        -> Result<(RootOwner, RootKind), HeapError>;
    pub fn open(&mut self, heap: &mut Heap, owner: RootOwner,
                kind: RootKind, len: u32) -> Result<RootRangeToken, HeapError>;
    pub fn read(&self, at: RangePosition) -> Result<Value, HeapError>;
    pub fn borrow_initialized(&self, range: RootRangeToken)
        -> Result<&[Value], HeapError>;
    pub fn write(&mut self, heap: &mut Heap, at: RangePosition, value: Value)
        -> Result<(), HeapError>;
    pub fn clear(&mut self, heap: &mut Heap, at: RangePosition)
        -> Result<(), HeapError>;
    pub fn copy_value(&mut self, heap: &mut Heap,
                      from: RangePosition, to: RangePosition)
        -> Result<(), HeapError>;
    pub fn move_value(&mut self, heap: &mut Heap,
                      from: RangePosition, to: RangePosition)
        -> Result<(), HeapError>;
    pub fn swap(&mut self, heap: &mut Heap, a: RangePosition, b: RangePosition)
        -> Result<(), HeapError>;
    pub fn move_to_host_root(&mut self, heap: &mut Heap, from: RangePosition,
                            owner: RootOwner) -> Result<RootToken, HeapError>;
    pub fn release(&mut self, heap: &mut Heap, range: RootRangeToken)
        -> Result<(), HeapError>;
    pub fn release_owner_step(&mut self, heap: &mut Heap, owner: RootOwner,
                              cursor: &mut RangeReleaseCursor, slots: u32)
        -> Result<RangeReleaseProgress, HeapError>;
    pub fn relocate(&mut self, heap: &mut Heap, capacity: u32)
        -> Result<RelocationCost, HeapError>;
    pub fn close(&mut self, heap: &mut Heap) -> Result<(), HeapError>;
    pub fn stats(&self) -> FrameSlotsStats;
}

impl Heap {
    pub fn budget_usage(&self) -> HeapBudgetUsage;
    pub fn gc_step(&mut self, budget: GcBudget)
        -> Result<GcProgress, HeapError>;
    pub fn gc_step_with_ranges(&mut self, budget: GcBudget, roots: RootRanges<'_>)
        -> Result<GcProgress, HeapError>;
}
```

`HeapBudgetUsage` is a clonable read-only view of the same live accounting book;
it reports charged/peak/remaining bytes without borrowing Heap. It cannot reserve
or refund bytes. This permits real allocator-boundary observation while a storage
method exclusively borrows Heap; a snapshot of HeapStats alone cannot do that.

`RootRanges` is a concrete read-only provider view. It is constructed only by
`FrameSlots::roots`, has no mutable-slice interface and cannot accept arbitrary
unbarriered storage. One active pool attaches to a Heap; every task's range
lives in that pool. It binds to that heap/epoch and an internal attachment ID.
The executor can borrow arguments through `borrow_initialized`; the borrow
ends before a write, relocation, release, or VM reentry. `RangePosition` is
a storage position, not a second bytecode Reg type. Consumers convert their
verified operand to its u32 offset; this task never defines bytecode operands.

Approved compatibility changes: `gc_step` now returns `Result`, and
`HeapLimits` gains `max_root_ranges` (default 1,024, independent of
single roots). Existing heap tests/load calls will check successful Results.
With any live external descriptor, provider-less `gc_step` returns
`MissingRootProvider` before changing any phase/cursor, including Weak/Sweep.
Both entry points use one collector implementation. `RootKind` gains Arguments
and Exception; existing TaskFrame, NativeContinuation and Delivery are reused.
`FrameSlotsLimits` is supplied explicitly; no implicit unbounded growth.

New errors distinguish invalid/stale range, wrong heap/epoch/provider,
missing provider, provider length mismatch, uninitialized slot, range/window
capacity, allocation failure and existing ResourceLimit/UnconfirmedLive.
`GcProgress.roots` remains the aggregate quota count; diagnostic subcounts
identify single-root slots, descriptors, holes and range Values. Their sum
never exceeds roots/units; no hidden descriptor traversal occurs outside them.
RangeReleaseProgress reports slots inspected, ranges revoked and completion.
RelocationCost reports descriptors/Values visited, bitmap bytes copied and
old/new charged residency. FrameSlotsStats and HeapStats report capacity,
active/free/retired range counts, external bytes and total/peak charged bytes.

## Internal interfaces and ownership

`root_ranges.rs` owns RangeKey/RootRangeToken and a Heap descriptor with
owner/kind/fixed length. RangeKey uses the existing slotmap retirement adapter.
`frame_slots.rs` owns one contiguous Value buffer, a separate bit buffer,
generation-checked range-to-base mappings and a bounded free-span table.
The pool keeps no per-register Box, object, RootToken, map entry or Rc.
Mapping/free-span metadata is per range and indexed by descriptor slot.
The heap stores no Value pointer, buffer, provider mapping or persistent borrow.

Internal methods only:

```rust
// Names may be private implementation details; semantics are mandatory.
Heap::register_null_range(owner, kind, len) -> RootRangeToken
Heap::unregister_range(token) -> ()
Heap::range_descriptor(token) -> descriptor
Heap::root_insertion_barrier(value) -> Result<(), HeapError>
RootRanges::resolve(token) -> Result<&[Value], HeapError>
Heap::reserve_external_bytes(bytes) -> accounted reservation
```

All raw buffers, mappings, registration, mutating indexing and budget release
operations remain private. No exported unchecked registration, root-barrier
escape hatch, IndexMut or mutable slice permits a caller to bypass barriers.
Every mutation validates the range's full key, provider attachment and heap.
The concrete provider's resolution checks the full generation and exact length.

The future VM owns both Heap and FrameSlots. FrameSlots holds a private shared
accounting/attachment lease; it is bookkeeping, not a GC root or a Value address.
Reservations use Heap's existing total ceiling, including its reserved and
payload bytes; external charges are visible in HeapStats. Mutation/allocation
requires the same Heap, so a pool cannot allocate using a different ledger.
GC borrows the provider for just that call; Rust excludes a simultaneous pool
write/relocation. Values and tokens remain in the existing single-thread domain.

Token copies do not create registration or retention. `release` invalidates
the generation before recycling a span. `close` unregisters all ranges before
freeing owned buffers and metadata. Dropping a pool without close while its
heap remains alive sets the attachment missing; remaining descriptors make
collection fail explicitly instead of silently treating them as empty. The
buffers are physically dropped before their accounting reservation is released.
Normal VM teardown closes the pool first. A new attachment is permitted only
after the old attachment is gone and has no remaining live descriptors; old
tokens cannot regain validity. A fresh Heap still invalidates all old handles.

## Publication, transfer and release order

1. Check limits, arithmetic, descriptor capacity and a contiguous free span;
   reserve required bytes/metadata before allocating anything.
2. Initialize that span to all null and clear its bitmap. Internally register
   the fixed-length descriptor and publish its provider mapping as one method
   with exclusive borrows, no callback, reentry or GC between those changes.
   Failures leave neither a live unmapped descriptor nor a consumed window.
3. Publish real values one at a time: validate the incoming object using the
   existing cutoff rules, run the insertion barrier, write base+index, then set
   the initialized bit. An initialized null differs from an uninitialized slot.
4. Copies/moves/results establish the destination first, then clear the source.
   Swap validates and shades both incoming values before either write. A move
   to the same position is a validated no-op. Failure before publication leaves
   the source rooted. `move_to_host_root` creates the host root before clearing.
5. A borrowed or copied bare result leaves its Delivery range registered.
   Delivery uses ordinary move/copy to another registered window or an explicit
   owning host root; abandonment uses release. No read method consumes a root.
6. On release/cancellation, unregister first, remove the mapping, then recycle
   the span. Old bytes in a free span are private nonroots and are cleared before
   its next publication. Retained buffers/metadata stay charged. Cancellation
   visits descriptor slots/holes with a persistent quota cursor; the owner must
   prohibit new roots while cancelling, as for existing single-root cancellation.
   A coordinator advances both the existing single-root cursor and this range
   cursor. No script/native callback runs during storage cancellation.

An initial window fill is bounded by max_window_len and is reported as actual
Value/bitmap work. Fixed-length windows never resize in place: a larger logical
window is opened, values are transferred through barriers, then the old one is
released. No default frame or continuation metadata is fabricated by this task.

## Collector cursor and weak cutoff

Retain the single-root cursor and add range descriptor slot, remembered full
RangeKey and element offset. Each descriptor or hole costs one root/work unit;
each Value, including null or an uninitialized null, costs another unit. A
zero-length range still costs its descriptor unit. No whole-slice scan hides
inside one unit. At each resumed range, compare the current key with the
remembered generation; reuse restarts descriptor work and element offset zero.
Removal clears remembered state before advancing. Insertion behind an already
completed cursor remains safe because every incoming Value goes through the
barrier. Pool relocation preserves logical offsets, so scan progress remains
valid even when physical bases change.

Resolve the currently scanned descriptor through the temporary provider borrow;
a live descriptor with a missing slice, wrong attachment or mismatched length
returns an invariant error, never an empty slice. A bounded step may already
have marked earlier roots before encountering an invariant error, but cannot
advance past the invalid descriptor or enter Weak. Concrete mappings can only
change through the atomic publication/release/relocation methods above.
Single roots, all range work and the gray queue finish before the existing
single weak-liveness cutoff. After the cutoff, publication still uses the
existing UnconfirmedLive check; valid generation alone cannot resurrect white
objects. Object invalidation, payload release queues and slot retirement remain
the existing storage behavior.

## Pool growth, relocation and byte lifetime

The pool starts at explicit initial_capacity. A full/fragmented pool returns a
capacity error; no write quietly reallocates it. `relocate` is an explicit
exclusive safe-point operation bounded by max_capacity. It creates a new
contiguous pool and bitmap, copies each live range in logical order and publishes
all new bases only when ready. ID, length, order, initialization state and owner
remain unchanged. Real destination Values use the insertion barrier; no borrowed
slice survives the operation. Refusal/failure keeps old storage/mappings usable.

Before allocating the replacement, charge old and new Value capacity, bitmap,
replacement metadata and any temporary relocation records concurrently. Keep
the old reservation until old allocations have actually been dropped. Persistent
descriptor indexes, free spans, initialization bitmap, attachment/accounting
bookkeeping, pool holes and retired slots all count. Shrinking or closing refunds
only physical allocations that have been freed; popping a frame refunds no
retained pool capacity. Shared allocation bookkeeping is charged once.
Checked products/bounds precede allocations on 32-bit and 64-bit targets.

Relocation/open/close are configured bounded storage operations, not hard
wall-clock guarantees or resumable executor instructions. Validation will report
their largest indivisible work/time, allocations and simultaneous residency.
If the configured maximum is too costly for a target's safe-point budget, the
caller reduces the configured limits; this draft does not claim device timings.

## Behavioral and cost evidence required by the reviewed contract

- Between every one-unit GC step, overwrite a previously scanned register with
  an otherwise white object; move/swap the sole unscanned child behind the cursor
  without weak upgrades or another operation accidentally shading it. Include
  controls that fail when the relevant barrier is temporarily removed.
- Open/pop windows around cursor positions, reuse a descriptor while partially
  scanned, and assert a large old offset cannot skip the new generation's first
  values. Test holes, empty ranges, null initialization and precise progress
  counts, plus reduced-generation retirement and wrong heap/epoch/attachment.
- Relocate during partial scans, retain aliases/initialization state, deliver to
  another range/host root, borrow bare delivery results, explicitly abandon,
  and cancel owners by quotas. Collect after every transfer and after final
  release; verify stored child content, immediate stale-token refusal and later
  reclamation. Exercise the original weak cutoff and pending-object behavior.
- Reject provider-less legacy calls whenever external roots exist. Internal
  fault injection covers missing mapping/wrong length: no weak transition and
  no scanning past the broken descriptor. Normal GC allocates nothing.
- Measure an actual numeric register loop at several window sizes with the
  existing allocation-counter dev dependency: zero loop allocations, no heap
  object/root growth proportional to registers, exact stored numeric results.
  Structural review checks direct Value/bitmap indexing and per-range metadata.
- Compare allocator-requested residency to charged residency for pool reuse,
  fragmentation, failed growth, retirement, successful relocation, cancellation,
  shrink/close/drop. Observe the real deallocation boundary, following D01's
  standalone System-allocator forwarding probe pattern: old/new capacity stays
  charged while physically live and capacity requests remain blocked then.
  Observer-only unsafe code remains outside the production crate; production
  retains unsafe_code=forbid and no new dependencies.
- Rerun the original heap suite in debug/release, original storage_load and its
  allocator-versus-charged checks on Rust 1.96.0, fmt/clippy, workspace tests,
  dependency tree, and i686 library/test/example compilation. Report commands,
  actual measured cost and limitations. i686 compilation is not device execution.

## Preparation evidence (before implementation)

Fetched all three requested commits with `git fetch origin <frozen> <D01> <VM>`;
read frozen section 22 with `git show`; created task/issue-17-frame-slots from D01
and merged VM without conflicts. Official rustup installed Rust 1.96.0 outside
the repository at /workspace/issue17-toolchain. No remote main mutation,
merge, Actions invocation or frontend import was performed.

Environment for baseline Cargo commands:

```sh
export RUSTUP_HOME=/workspace/issue17-toolchain/rustup
export CARGO_HOME=/workspace/issue17-toolchain/cargo
export CARGO_TARGET_DIR=/workspace/issue17-target
export PATH="$CARGO_HOME/bin:$PATH"
```

`cargo test -p aside-vm --locked` exited 0 on the combined baseline. The checks
ran real object graphs, swap/root insertion barriers, unified weak cutoff,
bounded pending-payload release, allocator reclamation and slot retirement.
The following commands also exited 0 on the combined, unchanged source baseline:

```sh
cargo test -p aside-vm --release --locked
cargo run -p aside-vm --release --locked --example storage_load
rustup target add i686-unknown-linux-gnu
cargo check -p aside-vm --all-targets --locked --target i686-unknown-linux-gnu
cargo build -p aside-vm --lib --locked --target i686-unknown-linux-gnu
git diff --check
```

The original seven load cases observed zero allocations inside GC calls, zero
live objects/payload after collection, and zero allocator bytes after Heap drop.
Each measured allocator peak was covered by charged peak. Maximum observed GC
call was 44.387 microseconds (100,000-element array); the one-unit binary-buffer
case had a 15.575 microsecond maximum. These are one-run baseline observations,
including allocator instrumentation, not device or wall-clock guarantees.
Raw output is retained in [ROOT-RANGES-BASELINE.txt](ROOT-RANGES-BASELINE.txt).
Measurements ran in this Linux x86_64 KVM container with five exposed CPUs,
reported model Intel Xeon Platinum 8573C, using rustc 1.96.0
`ac68faa20` (2026-05-25); this is not the N100/Vita reference hardware.
i686 library, tests and example were cross-checked and the library compiled;
no 32-bit binary or device was executed. These preparation measurements describe
the accepted pre-V02 baseline. The initial draft PR contained this contract and
baseline evidence; V02 implementation evidence is recorded separately in
[ROOT-RANGES-VALIDATION.md](ROOT-RANGES-VALIDATION.md).
