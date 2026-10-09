# L04/L05/L06 lowering contract (IR v1, bytecode v1)

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
does not provide an implementation. Native values remain a later callable task.

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
reads/writes are supported by L06. Pattern/spread/member-value
access, full script imports/linking, structural callable annotations, native
values, generator/thread/async and the broader language/library scopes remain
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
locals. Complete script imports, patterns/spread, unified callable/standard
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

Spread/rest, patterns, Member values/locations, optional chains (including a
nonoptional segment belonging to a chain), children, classes, full script imports
and unified callable/library work retain located rejection. Member is never
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

## Limits and verification

Work, vector/string capacity, blocks, instructions, tables and register counts
are charged before traversal, reservation or emission. Source/HIR and caller
link inputs are borrowed; new retained and scratch storage are accounted
together. Defaults are 16,777,216 work units, 64 MiB capacity, 1,000,000 records,
200,000 instructions, 100,000 blocks, 65,536 registers per function and depth 128.
Compiler and bytecode checks use independent limits; B01's verification allocation
limit is capped to the compiler's remaining capacity. `capacity_peak_bytes` is a
conservative upper bound, not an RSS measurement.

The lowering integration tests compile actual source through L03, inspect IR
and register liveness/order, verify with B01, encode/decode, disassemble and check
all source-map boundaries. They cover Cell/capture identity, trusted native
defaults and namespace links, cleanup regions, Context preservation, inferred
types, temporary reuse and located limit failures. They do not dispatch bytecode
or invoke a native host function. Execution acceptance remains pending V03.

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
target/release/examples/lowering_probe 80 nested 64
target/release/examples/lowering_probe 100 terminal
ASIDE_LOWERING_EVIDENCE=target/reel-artifacts/issue40/evidence target/release/examples/lowering_probe 1 evidence
```

The probe measures parse/analysis/lower/emit+verify/codec separately, checks the
IR and final-package retained allocation ledgers against `stats_alloc`, and prints work, capacity,
blocks, instructions and registers. Measurements and rejection cases are recorded
in `LOWERING_PERFORMANCE.md`; no VM execution measurement belongs to this report.
