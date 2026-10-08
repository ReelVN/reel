# V03 executor: current review state

The first public interface/state proposal was committed as `40d1b9d`, before the
executor implementation. Public Rust types are now isolated in [api.rs](api.rs).
The draft PR is https://github.com/jhq223/reel/pull/23, with the creation-time base
`task/issue-17-frame-slots` unchanged. The frozen language document is exactly
`c18db948554cdc278307617846060fe133922818`. Integrated dependencies: V02 `cceb151`
and B01 `8121429` (B01 independent cost revalidation is still coordinated by the
integration owner).

This executes verified hand-authored B01 packages. Source compilation/lowering,
creator-guide snippets and production source execution are **not** claimed.

## Public ownership and scheduling

`Vm::load` takes only VerifiedProgram and a caller-supplied trusted native
registry. The VM replaces the opcode list with its actual dispatcher list.
Native signatures/capabilities must come from independent adapter metadata,
resolved against the program's tables; they must not be copied from an untrusted
package. Tests and the probe independently declare the fixture echo adapter.

`start`/`start_closure` allocate bounded frames and copy externally borrowed
arguments before returning. `run_slice` drives ScriptFrame binding/body/exit
and NativeFrame copy/bind/issue/wait/callback/resume/commit/release stages.
A TaskId is fixed for the entire task, including native callbacks; FrameId and
host/delivery serials are monotonic. Internal argument/return targets are the
current frame or its immediate caller; no register objects or state HashMap exist.
Defaults run for every call in the callee closure's definition environment and
module, with preceding bound parameters as inputs and inherited NoWait.

HostCall arguments borrow a contiguous rooted window. Respond consumes the
exact issued ticket once. Continue and CallScript produce fresh resume tickets;
they never issue the original native entry again. Pending requires the scoped
`ticket.wait_id()` identity. Ready/Error completion consumes only the current
wait; old, repeated and cross-VM completions fail without committing a result.
The host can inspect continuation values and callback value/fault metadata.
Effects and Context are carried on tickets and inherited by callbacks. A native
that may wait can still return Ready in NoWait; actual Pending faults at the
original call location. Cleanup calls inherit NoWait dynamically.

Returned/Fault holds a delivery root. `delivery_value` keeps it registered;
`take_delivery_root` publishes to an existing host root before clearing delivery;
`discard_delivery` explicitly releases the result. A task cannot be restarted
while an unclaimed delivery exists. Heap allocations/mutations for host results
use the same total byte ceiling; collection always goes through `Vm::gc_step`
with the single FrameSlots provider.

`cancel(owner)` invalidates tickets immediately and enters hard owner teardown.
Subsequent slices release comparison/unwind/frame roots, then scan single host
root descriptors. Completion is `VmError::Idle` with `is_idle()==true`, which
means the task is gone. It does not run script finalizers. External native
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
and null branches, closure creation, ordinary Call/Return, ordered ArgsNew/
ArgsPush/ArgsNamed, Throw/Leave/EndFinally. The same Cell kind stores initialized
state, immutable mutability and code/module-versioned builtin type contracts;
generic field writes cannot bypass them. Closure code/capture fields are immutable.

Every other opcode, including unreachable code, is rejected during load. The
current core also rejects structural/named/function type definitions, tuple
constants, async/generator functions, rest/property-bag signatures, native script
defaults, and excessive register/signature sizes during load. Rethrow, packed
spreads, collection/class/member/pattern/iterator/task/await/yield handlers and
snapshot/restore remain follow-up work on this exact execution interface. Return
type, binding type, initialization and mutability checks happen at runtime.

**Outstanding shared-interface conflict:** B01 currently exposes only
`exception_handler(function, pc, consumed_region)` for throws. Its strict consumed
subtree semantics are used for exception continuation. Return/Leave require a
normal cleanup selector that does not choose catch and that can represent an
explicit target PC. V03 contains a clearly marked temporary adapter for that
selection; it must be replaced by B01's shared API before final acceptance.
No old "skip one region" exception semantics are reused. Nested-finally behavior
checks below are preliminary until that shared normal-exit API is connected.
Full exception cause envelopes and Rethrow are not yet provided by this core.

## Budget units and atomic boundaries

Every slice has independent instruction, internal work, allocation-byte and
caller-supplied monotonic deadline limits. Tests labelled budget=1 set
`instructions=1`; comparison/continuation stages additionally exercise work=1.
A caller may need to grant more work for a bounded atomic operation. BudgetYield
reports `required_work` and `required_allocation`, without performing that
operation. A repeated undersized grant does not silently overrun the budget.

Argument pairs, parameter search, binding/installation and continuation stages
consume separate work. Text/Bytes equality walks one byte pair per step; tuple
equality uses a capacity-charged explicit cursor stack and one shared root window.
String concatenation precharges copying and allocation and is bounded by
HeapLimits.max_mutation_bytes. Frame/window construction precharges initialization
and descriptor searches. Native response acceptance is outside run_slice and
bounded by max_arguments/max_continuation_values; it validates inputs before
root publication and ticket consumption. Host adapter side effects are outside
the interpreter's instruction budget.

V02 release's descriptor quota does **not** bound its Vec free-span shifts. Each
release is conservatively charged `4 * max_root_ranges + 8` work units, including
binary search/shift/coalescing overhead; cancellation releases only when that
whole bound fits. Single-root descriptor checks use one unit each. Exception
cleanup discards at most one superseded unwind root per internal step. Slice
work/allocation reports are conservative dispatch charges, not measured CPU
cycles; the probe separately measures actual elapsed time and allocator calls.
Deadlines are checked between stages; a stage's atomic cost remains the stated
bound. Heap, code capacity, window values/bitmap/metadata, frame/unwind vectors,
comparison cursor/root storage, capture/text scratch and VM inline control all
share HeapBudgetUsage. No continuation contains uncharged owned host closures.

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
duplicate-arguments, exception-regions and all CELL_CASES. Every execution uses
instruction budget 1 with GC interleaved. Additional checks cover five native
response rounds, callback faults and callback Pending on the same task stack,
NoWait dynamic inheritance, original wait source, changed definition-cell defaults,
rooted terminal delivery, stale/cross-VM tickets and waits, common budget failure,
owner cancellation, two finally layers and a normal nested try inside finally.
Numeric edges cover exact large int/float comparisons, NaN, negative zero,
checked overflow and strict bool. Long equality roots persist across work=1/GC.

[RESULTS.txt](RESULTS.txt) records one Linux x86_64 release run, including total
instructions/work, slice and call timing, actual allocator calls/bytes, shared
peak bytes and cancellation work. Timing varies with the host. The probe's hot
loop and 10,000 Ready/Pending calls allocate zero times after VM construction.
Tests also assert zero allocator calls for real dispatch and repeated frame/root
reuse. i686 is a compile check, not a linked 32-bit execution claim.
