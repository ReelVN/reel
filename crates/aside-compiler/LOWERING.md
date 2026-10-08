# L04 lowering contract (IR v1, bytecode v1)

Issue [#24](https://github.com/jhq223/reel/issues/24) consumes L03
`AnalyzedModule`; it does not analyze a second semantic model or execute programs.
The owner's language baseline is
`32054c99d8ba5e174657d844247a25aeb58a9989:docs/aside-language.md`, superseding
`7322292a32a416e6963e5b70162a25fb4e38e54a`. The branch retains real merge ancestry
from L03 `6f2329b3efe758a4bb8f52e469c25bcd396ff719` (PR #20) and B01
`57ff60e635b2f81f0ccbcf323ce420e45b908ce8` (PR #21). No V03 implementation is
copied or merged. Dependency acceptance remains the integration owner's decision.

Allowed edits: aside-compiler and its Cargo.lock dependency entry. The initial
dependency merge resolves language/creator documents to the owner baseline.
No VM dispatch, host execution tests, Actions, remote merge, ready transition,
release or acceptance comment belongs to this task.

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
native calls, forward shared Cells, and try/catch/finally/throw/rethrow. Ordinary
function loops and cleanup-aware break/continue are also supported. Uninitialized
bindings stay distinct from null. Aggregate/tuple/pattern/spread/member-value
access, full script-module initialization, structural callable annotations, native
values, generator/thread/async and the broader language/library scopes remain
deferred. An explicit author return escaping a default expression is diagnosed:
the shared default-thunk transfer contract does not yet represent its
enclosing-body target. Combined derived+init function context likewise has no
single B01 Context tag. These boundaries do not imply full-language completion.

## Limits and verification

Work, vector/string capacity, blocks, instructions, tables and register counts
are charged before traversal, reservation or emission. Source/HIR and caller
link inputs are borrowed; new retained and scratch storage are accounted
together. Defaults are 16,777,216 work units, 64 MiB capacity, 1,000,000 records,
200,000 instructions, 100,000 blocks, 65,536 registers per function and depth 128.
Compiler and bytecode checks use independent limits; B01's verification allocation
limit is capped to the compiler's remaining capacity. `capacity_peak_bytes` is a
conservative upper bound, not an RSS measurement.

The 17 lowering integration tests compile actual source through L03, inspect IR
and register liveness/order, verify with B01, encode/decode, disassemble and check
all source-map boundaries. They cover Cell/capture identity, trusted native
defaults and namespace links, cleanup regions, Context preservation, inferred
types, temporary reuse and located limit failures. They do not dispatch bytecode
or invoke a native host function. Execution acceptance remains pending V03.

Reproducible commands (Rust 1.96.0):

```sh
cargo fmt --all --check
cargo clippy -p aside-compiler --all-targets --locked -- -D warnings
cargo test -p aside-compiler --all-targets --locked
cargo test -p aside-compiler --all-targets --release --locked
cargo build -p aside-compiler --example lowering_probe --release --locked
target/release/examples/lowering_probe 1000 plain
target/release/examples/lowering_probe 10000 temporaries
target/release/examples/lowering_probe 1000 captures
target/release/examples/lowering_probe 1000 named
```

The probe measures parse/analysis/lower/emit+verify/codec separately, checks the
retained allocation ledger against `stats_alloc`, and prints work, capacity,
blocks, instructions and registers. Measurements and rejection cases are recorded
in `LOWERING_PERFORMANCE.md`; no VM execution measurement belongs to this report.
