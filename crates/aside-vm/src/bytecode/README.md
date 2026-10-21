# Shared bytecode loading and validation

`aside-vm::bytecode` defines immutable program data, encoding/decoding,
verification and disassembly. [FORMAT.md](FORMAT.md) describes the wire contract;
`spec.rs` is the shared instruction table. The executor separately advertises
implemented operations, so a representable instruction may still be refused at
runtime admission.

## Loading and integration

Tooling calls `decode(bytes, limits)` for bounded shape/index decoding, then `verify(package, limits)` (or `VerifiedProgram::decode`) for semantic package validation. `disassemble(&program, &mut sink)` writes a listing including binding initialization/storage, descriptors, regions, native versions and source byte spans. The validated package is immutable through the public API; `VerifiedProgram` construction is private.

Execution loading uses `VerifiedProgram::load(bytes, limits, &support)`. The trusted native adapter independently resolves its registration's parameter names/types into the package tables, then supplies the exact signature and `required_capabilities`. Every trusted native requirement must be authorized and declared in both the package and native record. Copying untrusted declarations into RuntimeSupport is not registration validation. Unsupported opcodes are rejected even when unreachable.

Decoding, package verification and runtime admission each enforce max_work independently. VerifiedProgram retains the supplied Limits for subsequent require_runtime calls. Repeated shared signature/parameter/capture scans, capability searches, native signature comparisons and registry string comparisons charge their upper work bound before the comparison or inner traversal. Opcode support is normalized once to a bounded lookup table. An error's source-map lookup uses the previously verified PC ordering and binary search.

Symbol uniqueness uses transient standard-library hashes and a reserved sorted index. Equal-hash groups undergo exact byte sorting/equality; even a forced all-collision group avoids all-pairs comparison. Generic u32 uniqueness checks likewise use a reserved sorted index. These sorts reuse Rust 1.96 BinaryHeap and its existing Vec allocation: each push is charged for at most floor(log2(len)) upward levels; in-place heapsort is charged for two comparisons per downward level before it runs. String fallback comparison bounds include the group's maximum byte length. Retained Vec capacities and simultaneous collision-group scratch are checked before reservation, measured, and released explicitly. Hashes never enter the wire format.

`InstructionSpec::visit_registers_budgeted` charges known window/list sizes before iteration and can reject scalar visits; `visit_registers` is the unmetered adapter for trusted analysis. Match outputs use ConditionalWrite. CellNew has exactly one operand, BindingId: its Cell storage register comes from BindingDef. Explicit cells start uninitialized and retain the fixed type/mutable contract through every alias. The runtime retains the TypeId's code/content version and definition ModuleInstance; GC release state is separate from initialization.

Parameter(i) accepts Value(Reg(i)) and Cell(Reg(i)), with exact signature name/type/mutability and a unique record per parameter (including rest). Missing records retain implicit Value storage. Each script owner/default prefix must have identical storage, and mutable prefixes require Cell. The binder initializes each invocation's parameters in order; only its internal default-prefix path forwards Cell identity. Ordinary calls to a default FunctionDef create fresh bindings. CellNew remains restricted to Explicit cells. No fields, tags, opcodes or wire versions changed.

The transient parameter index stores only declared bindings and one function-offset directory. Binding scans, tuple sorting, prefix traversal and binary searches are charged before work, with fallible reserved Vec capacity charged before allocation. Tuple sorting reuses BinaryHeap's Floyd construction and in-place sort; searches charge ceil(log2(len))+1, including the final comparison for non-power-of-two lengths. Allocator tests check the simultaneous index capacity and reject one-byte-short scratch before reservation. A 20-function/1000-parameter shared-signature package without binding records avoids a dense 20,000-slot map; a separate 100-owner/100-default-prefix package decodes under max_work=300,000 but is rejected during the repeated prefix validation.

NativeDef reuses Signature without a new wire table. The last default's longest prefix defines canonical native storage: every related thunk agrees, and mutable prefixes require Cell just as script owners do. Readonly prefixes can consistently choose Value or Cell; parameters outside all prefixes and rest remain Value. The verifier normalizes shared native signatures once, validates all prefixes before publishing, and retains only sorted (signature, parameter) Cell pairs. `VerifiedProgram::native_parameter_storage(native,index)` returns BindingStorage with Reg(index); `native_parameters_need_cells(native)` selects the ordinary native no-Cell path. These immutable queries allocate nothing; the executor accounts its execution query work. `PackageStats::derived_capacity_bytes` reports cache capacity already included in owned_capacity_bytes. Runtime admission automatically counts it, while encode's dropped validation cache is excluded from its output-buffer ledger. No thunk rescanning is needed per invocation.

`exception_handler(function, original_unwind_pc, consumed_region)` treats the cursor as a consumed lexical subtree and only returns strict ancestors. Two finally layers unwind inner → outer → completion. Rethrow starts its unwind at the rethrow instruction while retaining the original error source. Normal handler entry and cleanup bypass (including fallthrough) are rejected; nested Leave that remains within an enclosing finally is valid. EndFinally requires its matching active runtime unwind record.

`cleanup_handler(function, original_pc, CleanupDestination::{Return, Leave(target)}, consumed_region)` queries the next Finally for a normal exit and returns the same HandlerTarget (exception=None). Runtime keeps original_pc and destination fixed, then advances consumed_region after each EndFinally. It shares the exception selector's exact all-block strict ancestor relation and compares the containing protected/catch block widths, including nested finally layers inside an outer catch whose protected range is shorter. Leave preserves an enclosing source block containing its destination; an already active finally is never selected again. Invalid function/source/destination/cursor indices are rejected. Queries borrow verified metadata, scan O(region_count), and allocate nothing; the executor owns saved unwind records and execution work accounting. Return cleanup has a verified three-layer catch nesting regression (inner → middle → outer → None), and Leave has target-sensitive cleanup and enclosing-catch/finally preservation checks. Exception routing continues through exception_handler.

## Limits and checks

Default package counts and lengths use their encoded index widths and addressable
memory. Embedders may supply stricter `Limits`; structural validation, checked
arithmetic and allocation failures still apply. `PackageStats` reports retained
capacity and verification scratch separately from caller-owned encoded input.

```sh
cargo test --locked -p aside-vm bytecode
cargo test --locked -p aside-vm --test executor
cargo test --locked -p aside-compiler --test execution --test host_awaitables
```

The first command checks malformed input, instruction/descriptor contracts,
capacity accounting and codec round trips. Executor and source tests separately
exercise calls/defaults, cells, exceptions, wait/completion identity, cancellation
and interleaved GC. The host-awaitable source checks also capture and restore
waiting execution with registered adapters.

Historical loading and allocator measurements, original binary fixture sizes
and their test environments remain in the [fixed validation record](https://github.com/jhq223/reel/blob/f916ffedceb91465633fd549e4db1084c070edf3/crates/aside-vm/src/bytecode/README.md).
They describe those commits; the commands above check the current implementation.

## Argument contracts

Expanded compiler calls use existing ArgsNew/ArgsPush/ArgsNamed/ArgsSpread and
Packed, with empty CallDesc.names; ordinary calls still use Window. Rest remains
Signature.rest with the subsequent Parameter slot and matching binding contract.
It receives a fresh outer array of surplus positional values, including empty;
the type constrains that array, not its elements. A rest name cannot be supplied
as a named argument or silently redirected into PropertyBagTail. Property-bag tails remain a format-level contract awaiting executor support.

ArgsSpread shallowly captures array/tuple as positional values or own table
entries as named values at its source point. Table keys must be strings. Packed
name slots hold -1, a static SymbolId integer, or a GC-traced immutable string.
Dynamic names use resumable byte comparisons without copying or interning.
Positional values after named values fail at binding, including rest values. Invalid
source/copy/quota failures occur during capture; dynamic binding conflicts and
defaults occur after all actual arguments. Across budget slices, mutable array/table
capture requires an unchanged full content version, including element writes;
change fails with ConcurrentModification before target invocation. Captured
outer values are independent of later source edits while object references remain
shared. Partial builder/cursor/version state needs roots, capacity accounting and
failure cleanup. Script and native calls use the same resumable binder. No
wire-format change is made.
