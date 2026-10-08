# Aside storage (L01, contract 1.1)

This crate implements Value storage, an object heap, explicit roots, and one
incremental tracing collector. It does not implement the parser, bytecode
execution, language collection operations, native adapters, or snapshot format.
Aside depends only on slotmap, never on Reel crates. The bilingual creator guide
is maintained by the integration owner.

## Public interface draft

- `Value`: private explicit tag, full 64-bit scalar/key payload, and `HeapId`;
  16 bytes on both tested pointer widths. Copying a value performs no reference
  counting and does not create a root. Float storage preserves all IEEE bits.
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

Reproduction commands and actual results will be recorded in VALIDATION.md.
