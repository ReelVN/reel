# L04/L05/L06/L07/L08/L09 lowering contract (IR v1, bytecode v1)

Issue [#24](https://github.com/jhq223/reel/issues/24) consumes L03
`AnalyzedModule`; it does not analyze a second semantic model or execute programs.
The owner's language baseline is
`65f8ba9177ab81e817202fb56fdd92bab10fe6d0:docs/aside-language.md`, superseding
`7322292a32a416e6963e5b70162a25fb4e38e54a`. The branch retains real merge ancestry
from L03 `4500f788c5cd56bd431d6f75424f0c8a756a540d` (PR #20) and B01
`57ff60e635b2f81f0ccbcf323ce420e45b908ce8` (PR #21). No V03 implementation is
copied or merged. Dependency acceptance remains the integration owner's decision.

Issue [#37](https://github.com/jhq223/reel/issues/37) continues from main
`9e0e10b856a1a6995da0a9268b605cd508e3ad71`. Its allowed edits are compiler
semantic/lowering, static tests/measurements and technical documentation. The
integration owner maintains the language specification, index and both guides.
No VM dispatch, host execution tests, Actions, remote merge, ready transition,
release or acceptance comment belongs to this task.

Issue [#40](https://github.com/jhq223/reel/issues/40) continues from main
`6203ee138242baa74847f41c66e92382147aa5b1`, including L05. It adds fixed
collections and ordinary index locations inside the existing compiler crate.
Allowed edits remain compiler internals, static tests/probes and technical
reports; the owner synchronizes language/index/bilingual guide changes.

## Interface and behavior

`lowering::lower(&AnalyzedModule, &Linkage, Options)` produces immutable IR;
`IrModule::emit()` relocates it and returns a B01 `VerifiedProgram`, metadata,
entry function and resource statistics. `compile` combines these steps. `Linkage`
borrows a trusted verified package and exact symbol/node-to-native registrations;
the numeric package version comes from `Options::content_version`. Original
module/version/import/namespace/registration identities and source contexts remain
in `Metadata`; they are not reconstructed by matching names.

Symbol and name-use node links may both match a call. After each registration's
identity/signature checks, matches to the same NativeId coalesce; different
NativeIds remain a LinkContract error. The host seed uses the requested content
version independently of its native ABI version.

The IR reuses B01 instructions, bindings, captures, types and descriptors.
It adds ordered basic blocks, explicit terminators and exact source spans.
Parameters occupy fixed prefix slots. Lexical bindings retain stable slots;
temporary slots and contiguous call windows are reclaimed when their last
lowering use ends. The callee and all actual arguments are evaluated before
the single Call instruction. The binder, not the compiler, runs defaults.

Default functions receive the preceding parameters with the same binding
storage/type/mutability and the body's definition capture layout. Parameter
Cells are created by the call binder, never replaced by a body CellNew. Mutable
script parameters use Cells before later defaults. B01 derives and retains native
prefix layouts once; the compiler preserves that shared contract.
Native identities, ABI, capabilities and default implementations come from an
explicit trusted verified linkage package. An external default-presence flag
does not provide an implementation. Native values use a shared Native FunctionDef
and MakeClosure; the VM reads a rooted code constant and dispatches directly to
the native frame. Repeated reads allocate nothing and preserve identity.

Try lowering records shared exception regions and emits Return/Leave/EndFinally;
cleanup selection belongs to B01/V03. All emitted PCs have source maps.
Unknown lowering nodes and absent links fail with a source range, never a
placeholder null success. Author annotations supply runtime type contracts;
inferred function types do not create structural signature contracts.

Supported lowering includes ordinary functions/closures, scalar/string literals,
bindings/assignments, arithmetic/comparisons, return/tail values, if, strict boolean
short circuit, null coalescing, positional/named calls, defaults, trusted direct
native calls, forward shared Cells, and try/catch/finally/throw/rethrow. Module
initializer and ordinary function loops with cleanup-aware break/continue are supported. Uninitialized
bindings stay distinct from null. Fixed tuple/array/table and ordinary index
reads/writes are supported by L06; L07 adds nonoptional member values/locations
and dynamic calls through their results; L08 adds optional value chains.
Value match arms support names, wildcards, literals, grouping, type tests, OR and
NoWait guards. Tests reuse ordinary comparison/type/branch instructions; arm
bindings initialize once after acceptance, and each entry prepares fresh storage.
MatchFail reports MatchError through ordinary exception cleanup and source maps.
Structural patterns, optional-chain assignment targets, full script imports/linking, structural callable annotations, generator/thread/async and the broader language/library scopes remain
deferred. L03 rejects explicit source return in a parameter default with
InvalidDefault before producing an AnalyzedModule, including conditional and
short-circuit branches. Nested author functions return through their own boundary;
default expression/block tails produce a compiler-generated thunk Return.
Internal default loops and throw remain legal. The public analysis entry points
produce declarations representable by Ordinary/NoWait/Init/Derived. Combined
Init+Derived restrictions arise on actual call chains and belong to VM cumulative
flags; they are not an unsupported legal declaration in the current analysis API.
The compiler guard for a future unrepresentable declaration is retained.

While targets are installed before lowering the condition. Break records an exit
edge even when the complete condition terminates; continue targets the condition
head. Targets are retired before the next source statement/loop. Loops without an
exit edge do not fabricate a fallthrough path.

## Initializer-local bindings (L05)

The semantic `Symbol.origin` is authoritative for binding ownership. Only
ModuleGlobal enters ModuleDef.globals/exports; EntryLocal is allocated in the
module initializer's BindingDef table, with the existing SymbolId-to-BindingId
metadata. FunctionLocal keeps its source function owner; External host/import
metadata keeps the trusted-link boundary. `lowering::ir::Origin` still identifies
Module/Function/Default code bodies and has no new variant. Source function IDs,
bytecode format v1, CaptureSource and the executor interface are unchanged.

Each lexical entry prepares all its explicit slots before creating any named
closure: CellNew(BindingId) creates the uninitialized Cell, while Clear resets
a Value slot. Named functions then initialize their slots with MakeClosure and
BindInit; ordinary declarations initialize later. Mutable/forward/recursive
captures in the same entry reference the same binding, and intervening functions
forward CaptureSource::Capture. Reentering a block through a loop evaluates its
entry again, replacing the slot's Cell handle rather than reinitializing a prior
Cell. Fixed BindingId and register slots are reused; there is no dynamic binding
version, renamed global or frame-address upvalue. Catch retains its dedicated
CellNew/Clear + BindInit prefix before entering the body, avoiding double creation.

While condition scopes are per-entry too. Continue returns to the condition head;
all source-defined finally cleanup continues to use shared Leave/EndFinally
regions. Defaults receive the definition capture layout and the original bound
parameter prefix; they never recreate the initializer's definition-environment
Cells. Default-local bindings retain their existing call-local scope behavior.
Initialized immutable expression captures may remain Value captures.

options.enclosing is Local + Initialization::External, retaining real lexical
ownership/capture and external descriptor identity. No external Value/Cell link
exists in current Linkage, so lowering rejects it at its external declaration
range with MissingLink before allocating output. A native registration cannot
substitute for that environment. Trusted host/import native links remain supported.
NoWait-only ownerless declarations and non-root exports likewise retain located
rejections before table admission. Module-context block fragments use initializer
locals. Complete script imports, patterns, unified callable/standard
library and engine sessions remain separate tasks.

[Initializer contracts](tests/lowering/initializer_locals.rs) check actual source
through parse/analysis/IR/B01 verification/codec/disassembly, not only successful
verification. Static checks include:

| Source scope | Required structure |
| --- | --- |
| Root globals/exports and nested shadowing | Exact SymbolId metadata; initializer locals excluded from globals |
| Blocks, conditions, while bodies, catch | One static reset per used binding; reset dominates BindInit/read/capture |
| Forward read-only / mutual recursion | Uninitialized Cell before closure construction; later declaration initializes it |
| Sibling and transitive closures / defaults | Exact Binding/Capture source identity and complete Value/Cell layouts |
| Labels and finally | Correct condition-head destination and B01 cleanup selection; handler entry prepares its own binding |
| Enclosing / NoWait / nested pub / quotas | Located refusal before output admission or before the failing allocation/emission |

The dominance graph conservatively includes ordinary, shared exception/cleanup
and Leave-continuation edges. These are structural compilation checks: they do
not establish actual closure sharing, reentry isolation, GC/host restoration,
module publication or playable-story behavior. No V03/Aside execution is run.

## Fixed collections and index locations (L06)

`build::collections` lowers `()`, `(value,)`, multi-element tuples, arrays and
tables without spread. `(value)` remains grouping. Sequence elements are evaluated
once left to right into a reserved contiguous register window, then consumed by
the existing B01 Sequence descriptor and MakeTuple/MakeArray. Named table keys
become UTF-8 string constants; dynamic keys retain their expressions. Every key
then value is evaluated once in source field order into saved registers before
one MakeTable consumes the ordered Table descriptor. There is no intermediate
IndexSet, hash check or deduplication. The owner's contract puts key/hash
construction errors after expression evaluation. Later duplicate values overwrite
earlier values while retaining the first insertion position, according to ordinary
table semantics; this task establishes descriptor order, not runtime hashing or
duplicate-key behavior.

`build::locations` prepares either a Name identity or an Index receiver/key pair.
Ordinary index reads use IndexGet after receiver and key evaluation. Assignment
chains prepare all locations left to right, evaluate the final RHS once and emit
writes in reverse order. An index compound assignment prepares the location,
reads the old value, evaluates RHS, performs the binary operation and writes back
through the saved receiver/key. Neither operand is reevaluated. Nested index
receivers use ordinary IndexGet; the final assignment location has no redundant
read. Names keep their existing local/global/captured Cell write paths. Immutable
`let` still permits mutations through array/table indices; known tuple writes and
immutable-name rebindings remain semantic errors.

Saved windows/receiver/key registers stay reserved across later calls and are
cleared and reclaimed after their last lowering use. Terminal receiver/key/element
or RHS paths release compiler scratch without generating subsequent evaluation,
construction or writes. A remaining conditional fallthrough retains its saved
operands. Return/Throw and finally continue to use B01's existing shared regions
and cleanup selection. Work, descriptor/pair capacity, temporary locations and
register windows are admitted before traversal/reservation/emission. Quota errors
identify the failing collection, target or subexpression. IR/bytecode v1, VM
interfaces and RuntimeSupport are unchanged.

Collection spread, patterns, classes, full script imports
and unified callable/library work retain located rejection. L07 extends the
location machinery below with Member values/locations. Member is never
rewritten as Index. The existing trusted namespace-qualified direct native call
exception retains its exact linkage/provenance checks.

[L06 static matrix](tests/lowering/collections_locations.rs) uses actual source
through analysis, IR, B01 verification, codec and disassembly:

| Case | Structural evidence / negative boundary |
| --- | --- |
| Empty/unary/nested tuple/array/table and grouping | Exact constructor kinds, lengths and source spans; normal temporary clears |
| Element calls, named/dynamic/duplicate table keys | Ordered calls and Move definitions; saved inputs not overwritten by later calls; all pairs precede one MakeTable |
| Unhashable expression key | Compiles with all expression calls before MakeTable; no claim that construction succeeds |
| Ordinary/nested Index and mixed Name/Index chains | Receiver/key called once; common RHS register; reverse IndexSet/BindWrite/CellWrite/GlobalWrite order |
| All ten index compound operators | IndexGet precedes RHS call, operation consumes the old value, IndexSet uses original receiver/key |
| Defaults, initializer captures and closures | Shared capture/default layouts survive B01 roundtrip; compiler-generated default Return retained |
| Terminal receiver/key/element/RHS and finally | No unreachable calls, constructors or writes; shared Return/Throw cleanup targets; partial fallthrough use-def and conservative CFG dominance |
| Sequential collection/location expressions | Bounded register high water with 300 repetitions; explicit Clears before reuse |
| Deferred forms, tuple writes and quotas | Located preflight/semantic refusals; register/depth/work/capacity failure and exact-budget admission |

No instruction/value evaluator or execution simulation is used. Actual hash,
index bounds, alias mutation, GC, restoration and full engine behavior are untested.

## Member values and locations (L07)

Issue [#42](https://github.com/jhq223/reel/issues/42) builds on main
`fd12d1441fab0a06a5030e89001cc54abefebe6d`. It extends existing compiler
modules without new crates, descriptors, bytecode versions or VM execution.

`PreparedLocation::Member` retains an evaluated object register, a mapped B01
SymbolId and the source node. HIR NameId is not a bytecode symbol index.
MemberGet uses (destination, object, symbol); MemberSet uses
(object, value, symbol), unlike IndexSet's (object, key, value).
Ordinary Member and Index remain distinct instructions.

All mixed Name/Index/Member locations are prepared once in source order before
the single RHS; writes occur in reverse order. A simple final Member target has
no preliminary MemberGet. Intermediate receivers use ordinary expression reads.
The ten compound operations read the old member value before RHS, apply the
existing binary operation and write through the original saved object/symbol.
Later binding changes do not reevaluate a receiver. The saved location is not
an eagerly resolved slot or setter. Registers remain reserved across calls and
are cleared/reused after their final lowering use; terminating paths preserve
the existing cleanup/CFG rules without emitting pending writes.

MemberGet results feed CallTarget::Register. The callee is evaluated before
source-order arguments; the compiler does not inject self or classify arbitrary
callable values as methods. B01 retains its class-method binding contract and
table closures remain ordinary functions. IndexGet is not rewritten to
MemberGet, and this increment does not settle class string-index binding.
Class method classification and field/delegate/hook behavior remain separate.
Native values use the registration callable path described above.

Resolved namespace exports retain the trusted direct-native call exception:
identity, version, ABI, capabilities and actual default implementation checks
still apply. Linked namespace function values and optional calls use the same
registration; namespace writes and unlinked references are rejected.
L08 extends value chains below. Optional-chain assignment targets, class/super,
full module linking
and complete unified callable support remain outside this increment.

[Member static tests](tests/lowering/members.rs) inspect emitted operands,
Chinese symbol mapping, source order, mixed writeback, saved receiver identity,
all compound operators, dynamic-call arguments, initializer/default captures,
terminal and partial-fallthrough cleanup, native provenance and exact limits.
They use parse/analyze/lower, B01 verification, codec and disassembly only.
Actual receiver binding, setters/metamethods, aliasing, GC and restoration remain
unverified by execution.

## Optional value chains (L08)

Issue [#44](https://github.com/jhq223/reel/issues/44) builds on main
`3ad7354ccb0b0cae0d4f5a973ad24a71f6bdc998`. Existing HIR optional flags mark
explicit null guards; chain flags mark propagation through continuous postfix
Member/Index/Call segments. Group remains a value boundary. IR v1/B01 and the
VM interface are unchanged.

The chain collector walks the receiver/callee spine with charged work, capacity
and depth. It evaluates the base once and initializes a separate result register
to null. Each explicit optional segment emits BranchNull to a shared join; only
its non-null successor evaluates the index or source-order arguments and emits
the existing MemberGet, IndexGet or Call. Ordinary suffixes do not add guards.
Successful segment results update the evolving receiver through separate
temporaries, not the null-exit result. Only full fallthrough writes the final
result before joining. A later optional segment therefore also exits with null.

In a?.b[i()](arg()).c, a null a skips every suffix. If a is non-null but b is
null, ordinary later operations are not automatically skipped. In
(a?.b)[i()] and (a?.b)(arg()), the inner chain first joins; outer ordinary
operations still evaluate their index/arguments. An outer optional operator
creates its own guard. Nested chains have independent joins; null coalescing
branches on the completed chain result.

The call helper preserves a reserved callee register, source-order argument
windows and NamedArg offsets, with no injected self. If a receiver, index or
argument path terminates, no pending Get/Call is emitted on that path; earlier
null edges can still reach the join. The join remains in the surrounding
protected region and existing Leave/exception/finally behavior is retained.
No new catch layer masks missing members or hooks: non-null paths use the
original may-throw instructions.

A same-chain assignment target remains a located Unsupported result: this
increment does not define optional assignment or RHS-skipping semantics.
Group-separated outer ordinary locations such as (a?.b).c retain existing
location preparation and writeback rules. Optional native values/calls, including
native.run?() and native?.run(), use the linked native function value and the
ordinary optional-chain joins. Trusted native calls, including multiple Groups
and the prefix in native.run()?.x, retain
exact Member identity, version, ABI, capability and default-implementation checks.

[Optional-chain static tests](tests/lowering/optional_chains.rs) inspect null
initialization, guard counts and reachability, Group/nested joins, coalescing,
argument windows, rebinding, terminal/cleanup edges, native boundaries, UTF-8
spans, exact lower/emit limits and temporary reuse. Static structure does not
establish actual side effects, hook invocation, receiver binding, GC or recovery.

## Call spread and rest (L09)

Issue #46 / PR #47 builds on main 244bf5c7. No opcode, format, dependency or
crate is added. Unexpanded calls retain Window. Expanded calls evaluate the
callee once, allocate a private Args builder and append each source-order
ordinary, named or spread argument via ArgsPush/ArgsNamed/ArgsSpread. The final
single Call uses Packed; CallDesc.names is empty. Dynamic spread length does
not reserve an imaginary positional slot or become a compile-time flattening.

ArgsSpread occurs at its source position before later expressions. Arrays and
tuples supply positional values; own table entries supply named values with
string keys. Known invalid types diagnose; unknown types
retain runtime checks. Source/clone/quota failures belong to capture, while
dynamic name/slot conflicts, missing arguments and defaults belong to final
binding after actual arguments. The language contract requires shallow outer
capture and full array/table content-version checks across budget slices. The
executor roots partial builders and dynamic names, and shares resumable name
lookup and parameter binding across script/native calls. Named arguments must
follow positional values; duplicate names fail after all actual arguments.

Rest is separate from ordinary Signature.parameters and occupies the following
Parameter slot. The binder creates a new outer array including the empty case;
the annotation applies to this binding, not its elements. Incompatible known
annotations and named supply of rest reject. Mutable/captured parameters retain
their required Cell storage, and the body does not recreate binder-owned Cells.
Defaults retain only their preceding parameter prefix and definition captures.
ExternalSignature still has no PropertyBagTail projection; ordinary B01 tail
bag behavior is retained, while a rest-name collision cannot be silently routed
into it.

Callee and builder remain live across argument side effects. L08 null exits
skip builder/arguments/Call; terminated argument paths emit no pending append
or Call and preserve existing cleanup routing. Trusted native direct calls
retain exact symbol/node identity, version, ABI, capabilities and actual default
implementation. Native values also support these argument and optional-call
paths. Ordinary call syntax also forwards dynamic names: f(...positional,
...named). No separate forwarding builtin or new opcode is needed.

The source matrix in tests/lowering/call_arguments.rs verifies instruction
order, Packed descriptors, parameter/capture identity, optional and cleanup CFG,
negative boundaries, lower/emit quotas and register reuse through B01
verify/codec/disassembly. Fresh rest allocation, true shallow alias behavior,
ConcurrentModification, error timing, default execution and GC/restoration still
require runtime acceptance.

## Limits and verification

Work, vector/string capacity, blocks, instructions, tables and register counts
are charged before traversal, reservation or emission. Source/HIR and caller
link inputs are borrowed; new retained and scratch storage are accounted
together. Defaults are 16,777,216 work units, 64 MiB capacity, 1,000,000 records,
200,000 instructions, 100,000 blocks, 65,536 registers per function and recursive
depth 128. The depth budget is configurable without a hidden 128 clamp; flat
binary and postfix segments consume work and scratch capacity, not recursive
depth. Nested operands, parentheses and blocks retain their depth accounting.
Compiler and bytecode checks use independent limits; B01's verification allocation
limit is capped to the compiler's remaining capacity. `capacity_peak_bytes` is a
conservative upper bound, not an RSS measurement.

`lower`, `compile`, and `IrModule::emit` return `Box<Error>` on failure; `Error`
retains its kind, source span, detail, and complete statistics. Internal lowering
operations use the same representation. Only failures allocate an error object,
so successful recursive composition does not reserve a full error value for every
intermediate operation. Node dispatch and individual operations have separate
stack frames; flat binary and postfix chains use explicit work lists.

The lowering integration tests compile actual source through L03, inspect IR
and register liveness/order, verify with B01, encode/decode, disassemble and check
all source-map boundaries. They cover Cell/capture identity, trusted native
defaults and namespace links, cleanup regions, Context preservation, inferred
types, temporary reuse and located limit failures. They do not dispatch bytecode
or invoke a native host function. The separate `execution` integration test
executes emitted/decoded programs on the actual VM with tiny budgets and GC.

Reproducible commands (Rust 1.96.0):

```sh
cargo fmt -p aside-compiler --check
cargo clippy -p aside-compiler --all-targets --locked -- -D warnings
cargo test -p aside-compiler --lib --tests --locked
cargo test -p aside-compiler --lib --tests --release --locked
cargo build -p aside-compiler --example lowering_probe --release --locked
target/release/examples/lowering_probe 1000 plain
target/release/examples/lowering_probe 10000 temporaries
target/release/examples/lowering_probe 1000 captures
target/release/examples/lowering_probe 1000 named
target/release/examples/lowering_probe 1000 loops
target/release/examples/lowering_probe 1000 initializer
target/release/examples/lowering_probe 1000 sequences
target/release/examples/lowering_probe 1000 tables
target/release/examples/lowering_probe 1000 locations
target/release/examples/lowering_probe 1000 members
target/release/examples/lowering_probe 1000 optional
target/release/examples/lowering_probe 80 nested 64
target/release/examples/lowering_probe 100 terminal
ASIDE_LOWERING_EVIDENCE=target/reel-artifacts/issue40/evidence target/release/examples/lowering_probe 1 evidence
```

The probe measures parse/analysis/lower/emit+verify/codec separately, checks the
IR and final-package retained allocation ledgers against `stats_alloc`, and prints work, capacity,
blocks, instructions and registers. Measurements and rejection cases are recorded
in `LOWERING_PERFORMANCE.md`; no VM execution measurement belongs to this report.

## Executor integration boundary

Ordinary-function `await value` lowers to the existing B01 Await instruction,
with the expression's source span and normal register liveness. VM execution
accepts a trusted host Awaitable binding and enters the existing native frame,
including completed-result scheduling, pending waits, cancellation and fault
propagation. `cargo test -p aside-compiler --test host_awaitables` exercises source,
codec, ordinary/tiny budgets and interleaved GC. Generator yield and async
function lowering remain unsupported.

PR #49/#54 combines the executor and collection dispatcher with the compiler.
Native Windows debug/release checks execute fixed collections, Index/Member,
positional spread and script rest, including host entry. The `execution` test
independently compiles source, round-trips B01 and checks ordinary/tiny slices
with interleaved GC. B01 verification alone remains distinct from execution.
Tuple constants remain subject to a synchronous per-constant limit. Property-bag
tails, ArgsNamedSpread, delegates/hooks and other unsupported
forms still reject; registration cannot add missing handlers. These CPU checks
do not establish snapshot restoration, device behavior or performance bounds.

## Executable children composition

Trailing children blocks produce a named `children` array after the written
arguments. Standalone block calls may omit the semicolon; a following newline
ends that statement, so a parenthesized expression on the next line starts a new
statement. Ordinary calls and assignments retain their semicolon rules.

Collectors follow independent expression statements through ordinary blocks,
if/while/for control flow, and stop at value contexts and function boundaries.
Nested trailing blocks have independent collectors. `ChildrenPush` accepts only
host values declared composable and ignores null. `CallNoWait` carries the
children restriction into ordinary helpers without introducing a hidden closure
or changing return/capture semantics.

Array/tuple for uses `IterInit`, `IterNext` and `IterClose`; every iteration resets
its binding, and captured cells retain the corresponding iteration. The real
source execution matrix exercises composition, iteration, shared host properties,
normal and one-instruction slices with interleaved GC. Pure namespaces remain
compile-time identities; resolved native members retain their individual trusted
links, while taking a namespace as a runtime value is unsupported.

## Ordered collection spreads

Array literals with spreads use a private MakeArray builder, ArrayPush and
ArraySpread. Each spread captures array/tuple elements at its source position;
no register window proportional to the spread length is required. Fixed literals
retain their existing Sequence/Table descriptors.

Tables with spreads capture each source into a private table before evaluating
later expressions, then MakeTable consumes a TableSources descriptor containing
ordered Pair/Spread entries. Explicit key validation remains after all field
expressions. Later fields overwrite without changing first insertion order.
Temporary snapshots and builders are rooted registers and are cleared through
normal expression and frame cleanup. B01 adds descriptor tag 10 and opcode 005f;
there is no dependency or host ABI change.
