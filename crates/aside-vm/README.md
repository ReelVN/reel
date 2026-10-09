# Aside storage (L01 and V02)

This crate implements Value storage, an object heap, explicit roots, and one
incremental tracing collector. It does not implement the parser, bytecode
execution, language collection operations, native adapters, or snapshot format.
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
