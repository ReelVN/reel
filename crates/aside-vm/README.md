# Aside storage, bytecode and executor sources (L01, V02, B01 and V03)

This crate implements Value storage, an object heap, explicit roots, continuous
root ranges, one incremental tracing collector, and the B01 shared bytecode
format, codec, verifier and disassembler. See [B01](src/bytecode/README.md) for
program loading and validation boundaries. PR #49 integrates V03 executor sources from PR #23 onto the current baseline.
This integration has compile-only evidence, not independent execution acceptance.
It does not implement the parser, full language collection operations, platform
native adapters or snapshot format.
Aside depends only on slotmap, never on Reel crates. The bilingual creator guide
is maintained by the integration owner.

V02 implements the frozen section 22 at `1226c6b`: continuous FrameSlots,
generation-checked RootRangeToken descriptors and temporary RootRanges provider
borrows. The owner approved the [interface/ownership contract](ROOT-RANGES-DRAFT.md)
before implementation. See [V02 validation](ROOT-RANGES-VALIDATION.md) for current
commands, allocator-boundary evidence and measured costs; the L01 measurements
below remain historical evidence for the original heap.

`FrameSlots::new` binds one pool to a Heap with explicit initial/max capacity and
max window length. `open` publishes all-null storage and its mapping together;
`write`, `copy_value`, `move_value`, `swap` and host-root transfer preserve the
insertion barrier. `read` checks one initialized bit; null is an ordinary
initialized value. Values use a contiguous Vec, not per-register objects,
boxes, map entries or single roots. `borrow_initialized` checks the requested
whole window once for native arguments; its immutable borrow excludes mutation,
relocation and reentry. There is no mutable slice or unchecked public registration.

`Heap::gc_step` now returns `Result`; with any live external range it refuses
MissingRootProvider before Idle/Mark/Weak/Sweep or zero-work calls can advance.
Use `heap.gc_step_with_ranges(budget, frames.roots())`. Both entries run one
collector with descriptor/hole/Value quotas and a full-generation element cursor.
The original weak cutoff, object invalidation and retirement adapter are shared.

Release invalidates registration before recycling a span; retained capacity
keeps its charge. Physical relocation is explicit and keeps both pools and
bitmaps charged until the old allocations are freed. All storage, descriptor
indices, free spans, retired slots and control metadata share Heap's byte ceiling.
`HeapBudgetUsage` is a live read-only observer, including during real dealloc.
An unexpected drop with live descriptors makes GC and reattachment fail
explicitly. `close(&mut self, &mut Heap)` preflights before changing ownership;
an error retains the pool, while success permits a new provider. Closed inline
control metadata stays charged until that FrameSlots owner drops. The Rc byte
book itself stays charged while any observer/pool still physically owns it.
Future executor frame/continuation metadata must use this same internal budget;
this milestone allocates and accounts for window/provider metadata only.

## Public interface draft

- `Value`: private explicit tag, full 64-bit scalar/key payload, and `HeapId`;
  16 bytes in native tests and i686 compile-time assertions. 32-bit runtime
  verification remains pending. Copying a value performs no reference counting
  and does not create a root. Float storage preserves all IEEE bits.
- `HeapHandle`: full `KeyData::as_ffi()` u64 plus `HeapId` and `SessionEpoch`.
  Checked reimport uses the owning heap, never truncates the generation, and
  rejects foreign heaps/epochs. A restored session creates a fresh heap identity.
  This is an internal runtime handle, not a snapshot object number or C ABI.
- `RootToken`: explicit registration in one heap/session with a `RootOwner` and
  `RootKind`. `set_root` publishes through the insertion barrier; `release_root`
  relinquishes ownership. Tokens have no script finalizer. Rust locals containing
  Values or handles alone do not retain objects.
- `HeapLimits`: fixed object/root capacities and a total charged-byte ceiling.
  Preallocated slots, allocation/gray/release queues, root scan index, retired
  slots, chunk metadata and pending payload all count against that ceiling.
- `GcBudget` / `GcProgress`: independent root slots, mark edges, weak decisions,
  sweep objects, released edges and bytes, with a total work-unit cap.
  `begin_collection` and `gc_step` expose Mark -> Weak -> Sweep -> Idle.
  Mutation between steps is supported. Weak and Sweep share the same cutoff.
- `ObjectKind`: cell, array, tuple, table, string/bytes and mutable buffers plus
  storage records for class/instance/closure/generator/thread/Future/host/Set/
  Range/iterator. Record fields are strong edges; executable semantics are not
  claimed. `WeakRef` is a separately scanned weak field, never a strong edge.

All mutations pass through Heap methods; no mutable payload reference escapes.
Partial container scans retain an index. Swapping positions shades both moved
values before publishing; append/set and root changes use the same barrier.
New allocations during marking are gray; after the cutoff they are confirmed
live, and their validated initial edges only point to confirmed-live objects.

Constructors and mutation calls have explicit input limits. Large arrays and
buffers are built by repeated bounded appends. Chunked payloads release without
recursive graph destruction. Dropping the entire Heap is synchronous and may
free its reserved storage; there are no user callbacks. Scheduled cleanup uses
`gc_step`, including for deeply linked and cyclic graphs.

Dependency adapter provenance: slotmap 1.1.1 `src/lib.rs` KeyData packing and
`src/basic.rs` Slot/SlotUnion and remove_from_slot were inspected. Only the
adapter extracts generation/index for retirement and root indexing. Occupied
version 0xffffffff is retained as a Retired record rather than removed; tests
use a smaller threshold through the identical deletion path. No upstream source
is imported and no gc-arena dependency is linked; its mark/barrier/cutoff design
is an algorithm reference.

See [VALIDATION.md](VALIDATION.md) for reproducible commands, behavior evidence, measured load results and platform limitations.

HeapId is a process-global u32 allocated with AtomicU32 (no AtomicU64). IDs
1 through u32::MAX-1 are issued once; exhaustion returns IdentityExhausted
permanently before wrap. Heap, Value and scoped tokens cannot cross threads.

## Storage and work accounting

Defaults are configurable: 16,384 object slots, 1,024 root slots and a 32 MiB
charged-byte ceiling. Constructors/individual mutation calls accept at most 256
Values or 4,096 bytes by default. Allocation returns explicit capacity/resource
errors rather than extending queues; the host can drive maintenance and retry.
No method silently executes a whole collection to obtain space.

Cells store their one Value inside the reserved object slot. Other value fields
use 64-element chunks; text and bytes use 1,024-byte chunks. A chunk is charged
its 1,024-byte payload plus a conservative 1,024-byte tree metadata reserve.
The original reserve covered Rust 1.90 BTreeMap leaf/internal nodes with u32/Box entries
(up to 11 entries and 12 child links per node), including transient splits.
After removing the final chunk, the empty tree node is freed immediately.
Rust 1.96's original seven loads were rerun for V02 and still covered all observed
allocator peaks. Pinned RawVec/Global/Rc source and actual root-pool deallocation
were also checked. The accounting adapter must be rechecked with further
toolchain/dependency changes.

`HeapStats` includes actual queue lengths, reserved capacity, payload/pending
bytes, peaks and cumulative chunk allocations. Fixed slots stay charged when
vacant, pending or retired. Retired object/root slots reduce reusable capacity.
Root scan indices and object edge cursors use this preallocated storage. Weak
work counts allocation-record inspections, including records with no weak field.

Default `GcBudget` allows up to 256 total units per call, with separate caps of
256 root slots, strong edges, weak inspections, sweep objects and released edges,
and 16 KiB of released charged bytes. Empty-object/queue completion also costs
a unit. The host checks its clock between calls; quotas bound work, not a hard
wall-clock deadline. A byte cap below 2,048 cannot release a chunk and reports
`blocked`; partial edge teardown still advances separately. New allocations
append after the weak/sweep snapshot boundary and are left for the next cycle.
After logical invalidation, lookup fails immediately; pending chunks continue
to consume the byte ceiling until physically freed. Owner cancellation similarly
uses a persistent root-slot cursor with a caller-provided slot quota.

No graph destruction follows Rust references recursively, and collection does
not invoke user/script/native callbacks. Host records contain traced fields only;
registered adapter behavior and native resource ownership belong to later work.
Immutable strings/Bytes are available with bounded construction, and large text
or byte buffers are built by bounded appends. General Unicode editing, table
key operations, tuple equality/hash, copying continuations and snapshot codecs
are explicitly outside this storage milestone.

## V03 source integration and host lifecycle

PR #49 ports the PR #23 executor onto the current shared B01/compiler baseline.
Native Windows debug/release checks now execute the VM and the later #54/#55
collection/session integration. Source-to-codec-to-VM checks cover tiny budgets
with interleaved GC, calls/defaults, closures, exceptions and collections.
Run `cargo test --locked -p aside-vm -p aside-compiler --test execution` for the
source pipeline, or `cargo test --locked -p aside-vm` for VM contracts.

Vm::load takes a VerifiedProgram and independently trusted RuntimeSupport native
metadata. It enforces the dispatcher's actual supported opcodes across all
functions. The host explicitly starts the initializer/entry; load does not run
it. Do not reconstruct trusted registration by copying package signatures.
HostTicket arguments/continuations borrow Vm; copied Value/FaultId do not retain
objects. Preserve roots before mutable response, cancellation or GC.

Returned/Fault delivery retains its task and roots until explicitly transferred
or discarded. take_delivery_root publishes a successful value into an existing
root before discard. take_fault_delivery retains the complete fault graph,
which the host must later release. Reading a delivery does not release it.
BudgetYield is not terminal. ResourceBlocked preserves intermediate roots/state;
there is no live VmLimits growth API, and GC cannot remove active catch records.
Use Vm's provider-aware gc_step rather than raw Heap::gc_step.

Owner revocation is coordinated by the host. Vm::cancel(owner) cancels matching
tasks only, immediately invalidating hard-cancelled tickets/waits; later budgeted
slices reclaim task state. Idle means no matching task, not complete owner
retirement. Host roots can outlive every task, especially after fault delivery.

The host first marks the owner Revoked and rejects new tasks/roots/waits and old
completion delivery. Its bounded owner lifecycle record is reserved at owner
creation, not allocated without bounds during teardown. It captures cursor=0
and end=heap.root_slots(), then advances Heap::release_owner_step each budget
turn, passing slots = min(remaining work, end - cursor), since that API does
not accept end. Completion is cursor >= end. This scan releases single roots
only; host-owned non-task RootRanges require their own bounded release cursor.
Charge the actual cursor increment, including empty descriptor slots,
not the number of released roots. No-task owners still require this scan.
Retire only after task cleanup, the captured single-root scan, any host range cleanup and external adapter
cleanup complete. Invalidate host root records so later cleanup cannot double
release or reuse revoked FaultRoot handles. Reclamation of now-unreachable
objects remains separate budgeted GC work.

PR #54 extends this integration branch with source implementations for sliced
MakeTuple/MakeArray/MakeTable, IndexGet/IndexSet, MemberGet/MemberSet, tuple
constants, positional ArgsSpread and script rest (including host-start calls).
Native Windows debug/release tests execute the VM, collection and source pipeline. Source review found deferred
operand-fault and packed-argument admission defects; both fixes passed
independent source recheck. The checks cover runtime/GC/cancellation, but not snapshot restoration or devices.

Remaining unsupported forms include delegates/hooks, classes, structural tuple
types, property-bag tails, ArgsNamedSpread, collection spread, patterns, full
module/library binding and snapshots. RuntimeSupport cannot add missing handlers.

### Collection host access and execution source boundaries (PR #54)

sequence_len/get validate array/tuple kinds and bounds without exposing private
storage layout. table_own_string_lookup(table, key) returns a borrowed-key cursor;
table_own_string_step(cursor, work) reports Pending or Complete(Option<Value>) and
actual charged entry/byte/completion work. Only own string keys are considered,
without delegate or hook calls; missing differs from a stored null. The caller
retains the table through a native ticket or explicit root: the cursor owns no
root. Every step validates liveness and content version. Returned values require
normal explicit retention when kept beyond the borrowed operation. Legacy raw
alloc_record(Table) schemas are not language tables and are rejected by semantic
lookup, rather than interpreted as alternating private fields.

Language tables retain insertion order; overwriting an equal key replaces only
the value and advances content version. Sequence indices are nonnegative ints;
tuple mutation is rejected. Member and Index remain distinct B01 operations,
and ordinary stored closures do not receive an implicit self. Numeric key
comparison follows exact int/float equivalence; NaN and mutable nested keys
reject. UnhashableKey details contain a zero-based tuple element path (empty for
a top-level invalid key). Appended FaultKind IDs are OutOfBounds=14, MissingKey=15,
UnhashableKey=16 and ConcurrentModification=17; prior IDs and B01 v1 are unchanged.

Intermediate builders, comparison stacks and fault-detail paths retain roots
until publication or budgeted cleanup. ArgsSpread captures array/tuple elements
shallowly at the source argument position and validates source content version
until capture completes. Packed destinations must satisfy shape and version
invariants; both entries of each argument pair are preflighted/charged together.
A fresh outer rest array is prepared for every call, including empty and host
entry cases; existing default/type/Cell rules then apply. Runtime tests verify these contracts in native Windows debug/release builds.

Table lookup is resumable linear search: O(entries plus compared content), with
potentially quadratic repeated construction. No hash-index or scalability claim
is made. Runtime sequence construction is sliced; synchronous tuple constants
still obey the strict per-constant max_mutation_values limit, and loading remains
synchronous. Existing comparison-stack capacity limits still apply. Owner root
retirement and provider-aware GC responsibilities above are unchanged.
