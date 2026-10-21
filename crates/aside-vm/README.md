# Aside VM

This crate owns values, the object heap, explicit roots, frame slots, incremental
GC, the shared bytecode codec/verifier and the sliced executor. It also captures
and restores the VM object graph and execution state. The compiler and platform
adapters remain separate; this crate depends on no Reel or platform library.
See the [language specification](../../docs/aside-language.md),
[bytecode contract](src/bytecode/FORMAT.md) and
[loading guide](src/bytecode/README.md).

Current execution includes ordinary functions/defaults/closures, exceptions,
classes with inheritance and static members, collections and ordered spreads,
linked script modules, typed field writes, fixed tuple destructuring, host waits
and cancellation. Source
named spreads use ArgsSpread with own string-keyed table entries. The separate
ArgsNamedSpread opcode, remaining array/object/rest and match structural
patterns, delegates/hooks,
property-bag tails and native async/generator/thread execution remain unsupported.
Runtime admission checks the executor's implemented instruction set.

## Storage and ownership

`Value` has explicit tags and full-width scalar or generational key payloads.
Copying it creates no root. Rust locals holding values or handles therefore need
an explicit heap root when kept across execution, collection or mutable responses.
`HeapHandle` checks the owning heap and session epoch; restoration gives the new
heap its own identity. Values and scoped tokens stay on their owning thread.

Object identity is separate from compact entity storage. Retired objects release
entity pages while their generational handles retain identity protection.
Object/root/range storage grows from small buffers; hosts can set count and byte
quotas. Growth charges overlapping storage and prepares GC queues before
publication. Root changes, container writes and newly published edges share the
insertion barrier. Weak fields remain separate from strong graph edges.

`GcBudget` caps roots, edges, weak inspection, sweeping and physical release.
Long operations retain their cursor and roots across slices. `GcProgress`
reports actual work and blocked release; quotas do not promise a wall-clock
deadline. Heap pressure and explicit collection requests let the embedding
player avoid scanning a stable live heap after every low-allocation interaction.

`FrameSlots::new` binds one pool to a Heap with explicit initial/max capacity and
max window length. `open` publishes all-null storage and its mapping together.
VM loading adds the exact module-root count to `VmLimits.slots` execution
capacity. Constants, symbols, native values and globals use module admission,
not the per-operation window limit; their complete storage remains heap-charged.
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
Execution frames and native continuations use the same heap accounting.

## Execution and host lifecycle

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

Structural tuple/union contracts validate nested values, parameters, returns and
Cell writes with accounted scratch storage. Native function values are stable
rooted code constants and enter the ordinary native frame, including defaults,
waits and host callbacks.

`Vm::bind_awaitable(object, native)` attaches a trusted unary adapter to a Host
value. Await dispatches it with the object as its only argument;
`HostTicket::await_activation` identifies that execution. Ready resumes on a
subsequent VM run, while Pending uses the generation-checked wait/completion path.
Hosts retain terminal results and prune cancelled subscriptions using
`wait_is_pending`. A subscriber's cancellation preserves other subscribers and
the shared producer. Forwarded faults retain the producer envelope and the
consumer's await location.

## Snapshots

`VmSnapshot` captures the object graph, roots, execution frames, waits and native
continuations in slices. Hosts register versioned adapters for supported Host
payloads and rebuild external resources in the candidate session. Awaitable
bindings restore against that registered adapter and the same verified code.
Unregistered host state and mismatched code/adapter versions return explicit
errors. Capture and candidate restoration each expose progress and retain their
own roots until completion or cancellation. See `src/snapshot.rs` and the
source-level `aside-compiler/tests/host_awaitables.rs` round-trip checks.

## Collections and text

String methods expose byte/scalar lengths and slices, prefix/suffix checks and
separator-based joins. UTF-8 scans borrow bounded segments; copying and joining
use rooted private buffers and detect source mutation before publication.
The [text example](../../examples/text_operations.aside) runs through the same VM.

Collection methods include array insert/remove/resize/reverse/extend/clear/top/swap,
range slice on arrays and tuples, and table keys/values/entries/clear. Method
parameters accept their declared names, with conflicts checked before writes.
Bulk operations keep a rooted private buffer and source content versions across
slices. Success swaps payload ownership in constant work; the original identity
survives, and GC reclaims the old payload incrementally. A concurrent overwrite or
cancellation cannot publish a partial edit. Storage accounting moves with payloads;
GC restarts tracing moved edges using one queue entry per object.

### Collection host access and execution source boundaries

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
tuple mutation is rejected. Member and Index remain distinct bytecode operations,
and ordinary stored closures do not receive an implicit self. Numeric key
comparison follows exact int/float equivalence; NaN and mutable nested keys
reject. UnhashableKey details contain a zero-based tuple element path (empty for
a top-level invalid key). Appended FaultKind IDs are OutOfBounds=14, MissingKey=15,
UnhashableKey=16 and ConcurrentModification=17; prior IDs and bytecode v1 are unchanged.

Intermediate builders, comparison stacks and fault-detail paths retain roots
until publication or budgeted cleanup. ArgsSpread captures array/tuple elements
as positional arguments and own string-keyed table entries as named arguments.
Dynamic names remain GC-traced values and use allocation-free, resumable byte
comparisons. Script/native calls share the binder; builtins share name resolution.
ArgumentOrder=19 reports a positional argument following a named argument, after
all actual arguments are evaluated. Capture is shallow and validates source content version
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

## Collection capture checks

ArraySpread shares the resumable sequence capture with ArgsSpread. TableSpread
copies own entries and checks source/destination content versions between
slices. An empty table destination copies directly without repeated key scans;
merging into an existing table uses sliced equality against its original prefix
only. Newly copied source entries are already unique and are never rescanned.
Compiler-generated builders remain private until the expression completes.
Sources and nested values stay rooted across GC, faults and cancellation.

```sh
cargo test --locked -p aside-compiler --test execution --test semantic_quirrel --test quirrel_vm
cargo test --locked -p aside-vm --release --lib collection_ -- --nocapture
cargo test --locked -p aside-vm --release --lib table_merge -- --nocapture
```

The copy-cost check compares 1,024, 4,096 and 16,384 elements, asserting linear
charged work and exactly the required payload chunks. The table_merge check
separately verifies linear growth for a fixed two-entry destination prefix and
retained overwrite order. Its timings are local
measurements, not device or rendering performance claims.

## Checks and historical evidence

```sh
cargo test --locked -p aside-vm
cargo test --locked -p aside-compiler --test execution --test classes --test lowering_modules --test host_awaitables
cargo test --locked -p aside-vm --test gc_scheduling
```

The fixed [original heap measurements](https://github.com/jhq223/reel/blob/f916ffedceb91465633fd549e4db1084c070edf3/crates/aside-vm/VALIDATION.md)
and [root-range measurements](https://github.com/jhq223/reel/blob/f916ffedceb91465633fd549e4db1084c070edf3/crates/aside-vm/ROOT-RANGES-VALIDATION.md)
retain their tested commits, environments and allocator evidence. Run the current
tests for present behavior; these historical reports do not establish current
device or performance acceptance.
