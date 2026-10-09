# V03 executor: current review state

The first public interface/state proposal was committed as `40d1b9d`, before the
executor implementation. Public Rust types are now isolated in [api.rs](api.rs).
The draft PR is https://github.com/jhq223/reel/pull/23, with the creation-time base
`task/issue-17-frame-slots` unchanged. Language §22/§23 follows the integration
owner's document head `8dfa3bc`; §23 is the sole fault contract. Integrated dependencies: V02 `cceb151` and B01
`57ff60e`; independent dependency acceptance is coordinated by the integration owner.

This executes verified hand-authored B01 packages. Source compilation/lowering,
creator-guide snippets and production source execution are **not** claimed.
The three source findings on review head `4fef7b6` are corrected at `2501e9b`;
see [review changes and author evidence](REVIEW-FIXES.md).
The later Exit pre-admission scan finding is corrected at `1af4d31`; see
[charged selection and saved application](EXIT-BUDGET-REVIEW.md).

## Public ownership and scheduling

`Vm::load` takes only VerifiedProgram and a caller-supplied trusted native
registry. The VM replaces the opcode list with its actual dispatcher list.
Native signatures/capabilities must come from independent adapter metadata,
resolved against the program's tables; they must not be copied from an untrusted
package. Tests and the probe independently declare the fixture echo adapter.

One Vm owns one shared Heap, FrameSlots and module/global set. A bounded,
preallocated and capacity-charged TaskState pool holds independent stacks,
unwinds/caught faults, waits, deliveries, cancellation and equality cursors.
`start`/`start_closure` return a fresh TaskId and copy externally borrowed
arguments before returning. `run_slice(task, budget, now)` selects that Task and
drives ScriptFrame binding/body/exit
and NativeFrame copy/bind/issue/wait/callback/resume/commit/release stages.
A TaskId is fixed for the entire task, including native callbacks; FrameId and
host/delivery serials are monotonic. Internal argument/return targets are the
current frame or its immediate caller; no register objects or state HashMap exist.
Defaults run for every call in the callee closure's definition environment and
module, with preceding bound parameters as inputs and inherited NoWait.
Parameter preparation proceeds in order: create/initialize a shared parameter
Cell before the next default, pass it only through the internal prefix path,
and reuse it at body entry. Ordinary calls pass author Values and create their
own bindings, including ordinary calls to the same default FunctionDef.
Native defaults use B01's cached canonical layout and the same preparation
stages. Entry is published only after all parameters succeed. Shared Cells stay
in a private environment window; host_arguments borrows a separate continuous
author-value view, projected again before every resume ticket after callbacks.
Natives without defaults retain their direct Value path. A cached layout that
needs no Cell allocates no Cell environment, including read-only Value prefixes.
Native rest constructs an array of tail positional values.
Default frames retain their FunctionDef's lexical module for locals, captures
and diagnostics; shared parameter contracts separately retain the preparing
binder's module. Native has-cells flags are derived once at load into a charged
O(1) lookup table, so cost admission performs no binary layout search.

HostCall arguments borrow a contiguous rooted window. Respond consumes the
exact issued ticket once. Continue and CallScript produce fresh resume tickets;
they never issue the original native entry again. Pending requires the scoped
`ticket.wait_id()` identity. Ready/Error completion consumes only the current
wait; old, repeated and cross-VM completions fail without committing a result.
The host can inspect continuation values, callback values and complete faults.
With an active continuation, waiting completion resumes its current stage with
WaitingReturned/WaitingFault; otherwise it commits the native's terminal result.
Each resume ticket carries the continuation's version/stage, including callback
resumes. Effects intersect along calls; NoWait, Init and Derived accumulate as
independent flags on tickets and callbacks. A native
that may wait can still return Ready in NoWait; actual Pending faults at the
original call location. Cleanup calls inherit NoWait dynamically.

Returned/Fault holds a delivery root. `delivery_value` keeps it registered;
`take_delivery_root` transfers ordinary success, while `take_fault_delivery`
transfers the entire fault graph into an explicitly released FaultRoot before
clearing delivery and freeing the Task slot. `copy_delivery_value_to_root`
copies the original value while retaining the fault delivery;
`discard_delivery` releases the result and makes that Task slot reusable. IDs
include TaskId and heap/epoch/serial; old identities never identify a reused
slot. Other Tasks may start/run while an earlier Task waits or retains a result. Heap allocations/mutations for host results
use the same total byte ceiling; collection always goes through `Vm::gc_step`
with the single FrameSlots provider.

`cancel_task(task)` invalidates exactly that Task's tickets immediately and enters
hard task teardown. `cancel(owner)` selects all of that owner's Tasks and also
releases the owner's external single roots. Subsequent slices release that Task's
comparison/unwind/frame roots and bounded external descriptor scan. `VmError::Idle`
means the selected Task is gone; `is_idle()` checks whether any Task remains.
Selective cancellation keeps other Tasks and the shared module roots usable. It does not run script finalizers. External native
resource cancellation remains the owning adapter's responsibility; this core
retains no platform handles. Retained frame/window/work pool capacities remain
charged until the VM is dropped.

## Supported handlers and limits

The precise dispatcher list is `SUPPORTED_OPCODES` in api.rs; the executable
handler match uses B01 instruction_spec, with no second wire/opcode format.
Supported: load/move/clear, binding init/read/write, CellNew(binding)/read/write,
capture/global reads and global init/write, numeric/bitwise arithmetic and unary
operations, exact numeric/scalar/identity equality and comparisons, rooted
resumable String/Bytes/tuple equality, bounded string concatenation, strict bool
and null branches, closure creation, bounded MakeTuple, ordinary Call/Return, ordered ArgsNew/
ArgsPush/ArgsNamed, Throw/Rethrow/Leave/EndFinally. The same Cell kind stores initialized
state, immutable mutability and code/module-versioned builtin type contracts;
generic field writes cannot bypass them. Closure code/capture fields are immutable.

Every other opcode, including unreachable code, is rejected during load. The
current core also rejects structural/named/function type definitions, tuple
constants, async/generator functions, script rest/property-bag signatures,
and excessive register/signature sizes during load. Packed spreads, other collection/class/member/pattern/iterator/task/await/yield handlers and
snapshot/restore remain follow-up work on this exact execution interface. Return
type, binding type, initialization and mutability checks happen at runtime.

Normal Return/Leave use B01's `cleanup_handler(function, original_pc,
CleanupDestination, consumed_region)`; faults use its `exception_handler`.
Both use the same full lexical region relationships and strict consumed ancestors.
The executor has no separate cleanup selector. An inner catch/cleanup inside
an active finally preserves the outer pending exit. Only an escaping new failure
supersedes it, appending a typed ReplacedExit with the entire previous fault,
return value or versioned Leave target. Rethrow preserves the same envelope;
explicit throw in catch creates a new identity and CaughtFault edge. Binding
preparation failures go to the caller and skip unentered callee-body handlers;
an entered default body uses its own real catch regions.
Native binder faults retain their exact FaultKind; explicit ForwardFault preserves
identity, code, origin, stack and causes. Error(NewFault) always creates a new
fault, including when its value equals a prior fault's value. FaultBuild captures
one versioned stack entry per charged step before routing/unwinding. Runtime
language failures use private read-only CoreError values. Causes form a same-Heap
append-only graph, with event identities and a rooted cycle-aware diagnostic
cursor. FaultInfo Values retain the graph through normal roots and edges.
Actual catch departure frees its activation record after publishing propagation
roots. Native fault slots survive Continue/Pending under explicit Preserve or
Replace(ids), with an empty replacement marking faults handled. Hard fault,
catch or unwind capacity returns ResourceBlocked and retains the original
stage/roots. See [fault ownership and integration dependencies](FAULT-CONTRACT.md).

## Budget units and atomic boundaries

Every slice has independent instruction, internal work, allocation-byte and
caller-supplied monotonic deadline limits. Tests labelled budget=1 set
`instructions=1`; comparison/continuation stages additionally exercise work=1.
A caller may need to grant more work for a bounded atomic operation. BudgetYield
reports `required_work` and `required_allocation`, without performing that
operation. A repeated undersized grant does not silently overrun the budget.

Argument pairs, parameter search, binding/installation and continuation stages
consume separate work. Text/Bytes equality walks one byte pair per step; tuple
equality uses a capacity-charged explicit cursor stack and one root window per Task.
String concatenation precharges copying and allocation and is bounded by
HeapLimits.max_mutation_bytes. Frame/window construction precharges initialization
and descriptor searches. Native response acceptance is outside run_slice and
bounded by max_arguments/max_continuation_values; it validates inputs before
root publication and ticket consumption. Host adapter side effects are outside
the interpreter's instruction budget.

V02 release's descriptor quota does **not** bound its Vec free-span shifts.
For a transaction releasing n windows from f current free spans, the executor
conservatively charges `n * (4 * (f + n) + 8)` work, including binary search,
Vec shifts/coalescing and possible span growth between releases. Cancellation
releases only when that whole bound fits. Window construction separately
charges actual physical window lengths, current free-list search/removal,
and growth during partial-construction rollback. Configured descriptor capacity
remains fully charged as storage and is not substituted for live work.
Single-root descriptor checks use one unit each through the actual high-water
index, including sentinel and released holes. Exception
cleanup discards at most one superseded unwind root per internal step. Slice
work/allocation reports are conservative dispatch charges, not measured CPU
cycles; the probe separately measures actual elapsed time and allocator calls.
Deadlines are checked between stages; a stage's atomic cost remains the stated
bound. Heap, code capacity, window values/bitmap/metadata, Task/frame/unwind/caught
vectors, comparison cursor/root storage, capture/text scratch and VM inline control all
share HeapBudgetUsage. No continuation contains uncharged owned host closures.
Unsaved catch scanning and its release successor are precharged together;
the bound is computed from O(1) metadata before any predicate check. Granting
the reported retry requirement can advance that bounded action.
Exit admits the shared B01 region selector before its first query and saves
its result in the charged frame's ExitApply state. Selection costs
`regions * 8 + 1`, or `regions * 32 + 1` for consumed-region ancestry checks;
it is an atomic O(actual regions) action. Application separately admits catch
scan/removal, actual root release/open and any replaced-fault cause allocation.
Work/allocation/capacity retries preserve the selection, roots and stable event
without repeating the selector. Ordinary exits charge no hypothetical unwind
release; physical frame teardown remains separately charged.

## Verification and performance evidence

Commands from the repository root (Rust 1.96.0):

```
cargo test -p aside-vm
cargo clippy -p aside-vm --all-targets -- -D warnings
cargo check -p aside-vm --tests --examples --target i686-unknown-linux-gnu
cargo run -p aside-vm --release --example executor_load
```

[executor tests](../../tests/executor.rs) execute the exact final B01 constructors:
helper-native in both Ready/Pending modes, cell-closures, captured-default,
duplicate-arguments, exception-regions, all CELL_CASES, parameter-cell-default,
and native-parameter-defaults. Shared fixture execution uses instruction budget
1 with GC interleaved; parameter and fault construction also exercise work=1
and allocation=0 with explicit adaptive grants. Additional checks cover five native
response rounds, callback faults and callback Pending on the same task stack,
NoWait dynamic inheritance, original wait source, changed definition-cell defaults,
rooted terminal delivery, stale/cross-VM tickets and waits, common budget failure,
owner cancellation, two finally layers and a normal nested try inside finally.
Additional regressions keep A waiting while B mutates the same Cell/global,
retain multiple deliveries across GC, selectively cancel a third Task, and reject
old Task identities after slot reuse. NoWait+Derived and Init+Derived retain both
constraints. Callback dispatch uses the returned continuation stage, with no
host-side round counter. Ordinary preparation faults are routed at the caller;
hard fault/catch/unwind capacity returns ResourceBlocked with its phase and
roots retained. Inner errors inside finally preserve pending Return/Throw.
Normal exits nested in catch/finally use the shared B01 selector.
Numeric edges cover exact large int/float comparisons, NaN, negative zero,
checked overflow and strict bool. Long equality roots persist across work=1/GC.
Further regressions cover full fault identity/stack/cyclic causes and graph
delivery lifetime, continuation fault-slot replacement, 1,000 sequential catch
activations, cooperative cancellation during equality and actual parameter
preparation phases, native Entry gating, shared default closures, projected
host argument values after callbacks, and native positional rest.

At code head `1af4d31`, author validation passed 68 unit, 55 executor, 15
FrameSlots, 16 storage and one doc test, Clippy with warnings denied, and the
i686 compile check. This is author evidence; independent V03 execution remains
blocked in the separate acceptance workflow.
Private review regressions directly count rejected Release predicates and
runtime native query invocations, and execute Value-only fault diagnostics and
shared/local Cell captures across native default module boundaries.
Five additional Exit regressions count actual shared selector invocations,
verify exact-grant progress and saved results through work/allocation shortages,
hard capacity/GC and cancellation, preserve inner-catch pending Return and
check ordinary-exit cost. See [Exit author evidence](EXIT-BUDGET-REVIEW.md).

[RESULTS.txt](RESULTS.txt) links the seven-pair complete-fault
[native comparison](NATIVE-PERFORMANCE.md), seven additional native-default runs
and seven current live-work runs,
plus seven review-fix regression runs,
seven saved-Exit regression runs,
and retains the earlier historical
Linux x86_64 release run, including total
instructions/work, slice and call timing, actual allocator calls/bytes, shared
peak bytes and cancellation work. Timing varies with the host. The probe's hot
loop and 10,000 Ready/Pending calls allocate zero times after VM construction.
Tests also assert zero allocator calls for real dispatch and repeated frame/root
reuse. i686 is a compile check, not a linked 32-bit execution claim.
