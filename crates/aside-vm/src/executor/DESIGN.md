# V03 execution interface draft

Task ownership is corrected by [TASK-DRAFT.md](TASK-DRAFT.md); the original
single-task scheduling proposal is superseded. The implementation is being moved
to one shared-session Heap/module set with a bounded TaskState pool.

Contract: `docs/aside-language.md` at c18db948554cdc278307617846060fe133922818 §22.
Dependencies: V02 b14e393; B01 178df78, to be replaced with the integration owner's final reviewed head before acceptance. PR base is `task/issue-17-frame-slots`, selected at creation and retained.
Allowed edits: executor, existing language Cell/payload and necessary heap operations, library exports, executor tests/probes. No second opcode format, parser, or creator-guide edits.

## Public interface

- `Vm::load(VerifiedProgram, RuntimeSupport, VmLimits, SessionEpoch)` owns immutable code, Heap, FrameSlots and bounded metadata pools. The actual dispatcher replaces `support.opcodes`; load rejects every unsupported instruction, including unreachable code, and unsupported descriptor/type/function forms.
- `start(FunctionId, &[Value], RootOwner, Context)` / `start_closure(Value, &[Value], RootOwner, Context)` starts one task. Program/module identity and code version remain fixed. New work starts only after explicit terminal delivery disposal.
- `run_slice(SliceBudget { instructions, work, allocation_bytes, deadline_ns }, now)` returns `RunOutcome::{Returned(DeliveryId), BudgetYield(SliceUsage), HostCall(HostTicket), Waiting(WaitId), Fault(DeliveryId)}`. Internal stages consume work independently of bytecode instruction count; deadline uses caller-supplied monotonic time.
- `host_arguments(HostTicket)` borrows a rooted contiguous slice until the next mutable VM call. `respond(HostTicket, HostResponse)` consumes exactly one outstanding ticket. Responses: Ready(Value), Pending(WaitId), CallScript { closure, arguments, continuation }, Continue(Continuation), Error(Value). Continuation contains version/stage and a bounded strong-value slice copied into FrameSlots before acceptance. No owned host closures or opaque uncharged maps.
- `complete(WaitId, Ready/Error)` settles only a current wait, exactly once. `native_input(HostTicket)` exposes callback Ready/Error or continuation-stage state when a fresh resume ticket is emitted; the native entry is never reissued.
- `delivery_value(DeliveryId)` reads a bare Value while retaining its delivery root. `delivery_fault` also returns source/kind metadata. `take_delivery_root` transfers to an existing host root before clearing the delivery slot; `discard_delivery` releases explicitly.
- `cancel(owner)` enters a resumable cleanup phase; no more native tickets become valid. Subsequent slices drain frame/root metadata under the same work budget. The descriptor scan and each release's O(max_root_ranges) free-span work are conservatively charged, rather than counted as one constant unit.
- `gc_step` always supplies FrameSlots roots. Heap access permits allocation for host results, but GC runs through Vm to preserve the provider invariant.

## Internal states

A capacity-charged contiguous frame vector contains `ScriptFrame` / `NativeFrame`; a separate charged unwind vector supports nested cleanup. Frame IDs and host serials are monotonic, never reused. No Value is retained solely in metadata: closure/receiver, evaluated arguments, exceptions, callback outputs, continuation and deliveries live in root windows.

`ScriptFrame`: id, CodeRef(function/content version), ModuleInstanceId, PC, register/argument/context windows, binding cursor, return target (register/default/native/delivery), inherited effect mask/context, owner, current operation. States: Bind(arguments already evaluated) → Default(child frame with callee capture/module environment) → Execute → Unwind → ReturnCommit → Release.

`NativeFrame`: id, versioned native/signature, original caller PC/source, return destination, rooted bound arguments/continuation/result, inherited effects/owner. States: Issued → Responded → Waiting / Callback / Continue → ResumeIssued → Commit → Release. Each resume uses a new ticket; stale, cross-VM and repeated tickets fail without side effects. Pending in NoWait faults at the original call; an effectful signature may still return Ready synchronously there.

`Unwind`: origin PC, reason Return/Throw/Leave, rooted payload, active RegionId, parent record. The final B01 shared unwind selector chooses catch/finally and consumed regions. EndFinally matches the active record; nested normal try/finally inside cleanup pushes another record and preserves the outer one. New errors replace the relevant pending exit and skip consumed cleanup. Calls within finally inherit NoWait.

Cell retains the existing ObjectKind::Cell. Payload holds initialized state, immutable mutability and versioned type contract; all writes, including generic set_field, enforce it. BindInit publishes once after type checking/barrier. null is initialized data. Captures preserve cell identity; defaults inherit the callee closure's definition environment.

## Evidence required

Execute B01 helper-native, cell-closures, captured-default, duplicate-arguments and exception-regions fixtures, with instruction/work budget=1 and GC between resumptions. Extend runtime tests for multiple native callbacks/Continue, callback faults, NoWait dynamic wait, nested finally, stale tickets and rooted delivery. Measure real hot-loop allocation, repeated calls/root reuse and cancellation bounds; compile i686. These are hand-authored verified bytecode executions, not source execution. Complete language and snapshot features remain follow-up handlers through this same interface.
