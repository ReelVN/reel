# V03 fault implementation and follow-up interfaces

This is implementation status for language §23, not another language or opcode
contract. The integration owner maintains the specification and creator guides.
Current executable coverage consists of verified hand-authored B01 fixtures and
executor author self-tests. Source lowering and independent acceptance are not
claimed; the independent V03 test run is blocked outside this workspace.

## Graph ownership

`FaultId` encapsulates the full heap/epoch/generation handle. It is a borrowed
identity; copying the Rust ID/FaultInfo does not register a root. A rooted
`FaultInfo::as_value()` is the same-Heap, read-only envelope view. `FaultRoot`
owns one host root and must be explicitly released through Heap. The private
Envelope/Stack/Cause/CoreError kinds reject generic record construction and
field mutation. The original thrown author value retains its ordinary mutability.

The envelope's value, code, origin and original stack freeze at completion.
Appending a cause publishes a new immutable list node; old nodes remain
unchanged. Every edge passes through the Heap insertion barrier. A stack node
stores one bounded versioned entry; a cause node stores its stable event, typed
exit/relationship and linked full fault or return value. All private nodes fit
the preallocated Heap object slab and count toward its object capacity. The
larger slab slot, GC queues, frame metadata, scratch/cursor capacities and root
bitmap remain charged to the same byte ledger. Logical node allocation also
uses SliceBudget allocation bytes even though it needs no system allocation.

Every node holds an Rc CodeLease with the actual VerifiedProgram and its byte
reservation. A frame/FaultBuild uses the VM's lease; completed graph nodes keep
it alive independently of the VM's strong reference. GC drops each node's lease
after its three traced edges have been released in charged steps. The last
strong reference currently releases the entire program/reservation synchronously.
The live Vm holds its own strong reference, so current public VM GC cannot be
that last release. Hot reload or independently surviving retired versions must
add sliced/controlled code retirement; one GC object quota does not bound whole
package destruction. The private graph lifetime test proves reference release,
not bounded final-version destruction. Versions are not merely
integer labels in diagnostics. Live-reload registration remains a separate
module integration task; this VM currently loads one immutable package version.

`fault_stack` and `fault_causes` are bounded borrowed cursors; retain the envelope
while traversing. `FaultGraphCursor::new` owns a root and preallocated, charged
identity/work buffers. Its step charges each list edge and visited-ID comparison;
cycles/shared faults produce reference items instead of recursion. Traversal
capacity errors keep the cursor/root. Explicit cursor release frees both.

## Resumable faults and native adapters

FaultBuild freezes the failing stack before any failing frame is removed. Its
reserved Task state progresses through CoreError value, envelope, individual
stack records, cause, and completion. A partial graph stays rooted but cannot be
queried through public fault IDs. Each phase checks work/allocation quotas before
mutation. Stable build/replacement events survive retries. Completed roots are
published to catch/native/delivery before partial or source roots are cleared.

ResourceBlocked is a reserved control notice, never a replacement NULL error.
It reports a known next-step minimum and preserves the stage and side effects.
Object exhaustion can be resolved by explicit GC/release. Native fault slots can
be released with Replace(ids), including `replace_native_faults(task, frame, ids)`
when a new callback fault is blocked before frame release. Fixed load-time
capacities cannot currently be increased; if no release can resolve exhaustion,
the host can hard-cancel and drive bounded teardown. Other Tasks remain runnable.

Error(NewFault) creates a new graph using the original native call location.
ForwardFault accepts only a complete fault currently retained in that native's
fault slots or continuation Values; equality of raw thrown values never forwards
identity. Preserve/Replace is explicit on Continue and CallScript. Pending keeps
both continuation Values and fault slots. Waiting completion with an active
continuation issues WaitingReturned/WaitingFault at the current version/stage,
which permits continued algorithm steps; a plain native wait commits directly.
`host_resume_value(ticket)` reads the original value of CallbackReturned,
CallbackFault, WaitingReturned or WaitingFault; the old host_callback_value
name remains an alias. Ticket validation precedes reads, so consumed/replaced
tickets cannot read a later resume's value.
Both completion modes consume the wait once. New callback/continued-wait faults
must be published to a fault slot before releasing their source. Ready publishes
its result before native teardown releases all retained faults.

Catch records contain FrameId/RegionId/activation and two roots for original
value/full fault. The VM publishes Rethrow, new causes and pending exits before
removing a catch actually exited by the target PC/frame release. Helpers keep
the parent's catch active. Sequential catches reuse capacity. `error_info(task)`
observes that Task's most recent active catch and rejects Derived with Effect.
Hard owner cancellation releases control/root state without script cleanup;
`cancel_cooperatively` injects a complete, catchable versioned Cancelled CoreError.

## Required follow-up integrations

The standard-library owner must register/bind `core.error_info()` to this VM's
contextual query, convert Some(info) to Ready(info.as_value()) and None to NULL,
and use `respond_language_error(ticket, Effect)` for a Derived-effect diagnosis.
It must expose value/code/origin/stack/typed causes and CoreError code/message/
read-only details through read-only adapters. Generic mutable fields are not an
implementation of these views. This patch supplies the primitives; it does not
claim the source-level standard-library symbols are bound.

Await/async/generator handlers are not dispatched and are rejected at load. Their
owners must use the same FaultBuild/root/capacity rules. Adoption forwards the
producer identity. Each actual await consumer creates its own origin/stack and
an AwaitedFault edge with a stable activation; producer value/code are preserved.
Close metadata distinguishes generator/thread and logical ID/activation.
These future cause types are represented but not emitted by current handlers.

Snapshot/restore is not implemented. Its codec must serialize graph identities,
shared/back edges, versioned code registry/program/native references, immutable
stack and cause records, append event identities, active catch activations,
pending exits and original routing/consumed regions, FaultBuild phase/cursor and
partial graph, native fault slots/continuation phase, and unclaimed deliveries.
Restore allocates fresh Heap IDs before wiring edges; missing versions fail at
their recorded source. A snapshot stage boundary must preserve whether a cause
event has committed, so restoration never appends it twice. Diagnostic cursors
provide bounded graph traversal, not a snapshot codec or restoration proof.

`invoke_sync` remains a follow-up scheduling adapter on this same Task/run_slice
interface. It must enforce cumulative instruction/work/allocation/deadline hard
limits across yields, return owned success/fault, and map ResourceBlocked to a
host control failure while driving bounded cancellation. Already committed side
effects remain. Repeated ordinary run_slice grants are not invoke_sync.

Parameter-default Cell identity is a separate pending §22 integration: preceding
parameter Cells must exist before later defaults and be shared with the body.
The executor will consume the integration owner's final B01 Parameter Cell
layout verification change, rather than duplicating its verifier.
