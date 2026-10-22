# Aside VM

`aside-vm` owns values, heap storage, explicit roots, incremental GC, frame slots,
bytecode validation, sliced execution and snapshots. Its interfaces are defined
by the [language specification](../../docs/aside-language.md) and
[bytecode format](src/bytecode/FORMAT.md).

## Loading and roots

[`bytecode`](src/bytecode/mod.rs) decodes and verifies packages into immutable
`VerifiedProgram`. Execution admission also requires independently trusted
`RuntimeSupport`: native identity/signature/ABI/capabilities and the executor's
implemented opcodes. Start module initialization and the selected entry explicitly.

Copying a `Value` creates no root. Host values retained across execution,
responses or GC need an explicit root. `FrameSlots` provides initialized
register windows and GC ranges; provider-aware `Vm::gc_step` traces them.
Heap handles include heap/epoch identity. Root/container writes share insertion
barriers, and storage growth accounts overlapping allocations before publication.
See [`heap`](src/heap) and [`frame_slots`](src/frame_slots.rs).

Literal defaults share program constants. Aggregate constants materialize through
the sliced constant loader; expression defaults execute with preceding parameter
bindings and definition captures. Both use the same type/mutability checks.
Collections retain private rooted builders and content versions across slices;
success publishes a complete edit, while mutation conflicts or cancellation
release the pending work. Table lookup uses sliced linear equality search.

## Host execution and retirement

[`executor`](src/executor) keeps arguments, continuations, waits and deliveries
rooted. `BudgetYield` preserves execution; `ResourceBlocked` preserves its pending
state. A returned/fault delivery retains roots until the host transfers or
explicitly discards it. Borrowed arguments and copied fault handles follow the
same root ownership rule.

Awaitable Host objects dispatch through registered unary native adapters.
Completed results remain scheduling boundaries; pending results use generation-
checked waits. Host callbacks share native frames, inherited effects and sliced
cancellation. `finish_host_callback` returns a rooted value through the original
native boundary; cancellation retires the same region without running finally.

The host revokes owner admission and business waits before cancelling tasks.
Bounded task cleanup, the frozen single-root scan, host RootRanges and external
resource retirement must all complete before owner reuse. Scan work includes
empty slots; unreachable heap objects are reclaimed by separate GC slices.

## Snapshots

[`snapshot`](src/snapshot.rs) captures roots, object identity, frames, waits and
continuations incrementally. Versioned host adapters restore external payloads
against the same verified code. Capture/restoration own their temporary roots
until success or cancellation. Tuple constants may share storage; their identity
is unobservable and weak constant caches rebuild during restoration.

## Checks

```sh
cargo test --locked -p aside-vm
cargo test --locked -p aside-compiler --test execution --test classes --test lowering_modules --test host_awaitables
cargo test --locked -p aside-vm --test gc_scheduling
```

Public APIs carry their ownership and budget contracts in source. Source
execution tests exercise the compiler and VM together; snapshot and allocator
checks cover suspended state, release and actual storage accounting.
