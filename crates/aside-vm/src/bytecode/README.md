# B01 shared bytecode validation evidence

Implemented in `aside-vm::bytecode`; format contract and all 77 opcode entries are in `FORMAT.md` and `spec.rs`. This is program data, verification and disassembly. It contains no dispatcher, frontend dependency, heap mutation or GC/root-range code.

The branch combines accepted D01 `c03226a` and storage `95a7e84`. Owner language clarifications `999a8e3`, `c18db94` and `9cfe5ec` are preserved. The complete `docs/aside-language.md` from `32054c99d8ba5e174657d844247a25aeb58a9989` was imported verbatim after confirming the local file equaled the previous `7322292` document; its blob is `6d5a9515a9a20e6ab7a4f31ff327c099ad030dea`. This includes the script/native parameter-binding clarification and §23 runtime targets, without importing executor implementation. The initial interface checkpoint was `178df78`; `fb3c1fc23f5f34c5a73b02af967bbc97f296b007` closed the initial five reported regressions. The follow-up shared-graph work accounting and its evidence are described below.

## Loading and integration

Tooling calls `decode(bytes, limits)` for bounded shape/index decoding, then `verify(package, limits)` (or `VerifiedProgram::decode`) for semantic package validation. `disassemble(&program, &mut sink)` writes a listing including binding initialization/storage, descriptors, regions, native versions and source byte spans. The validated package is immutable through the public API; `VerifiedProgram` construction is private.

Execution loading uses `VerifiedProgram::load(bytes, limits, &support)`. The trusted native adapter independently resolves its registration's parameter names/types into the package tables, then supplies the exact signature and `required_capabilities`. Every trusted native requirement must be authorized and declared in both the package and native record. Copying untrusted declarations into RuntimeSupport is not registration validation. Unsupported opcodes are rejected even when unreachable.

Decoding, package verification and runtime admission each enforce max_work independently. VerifiedProgram retains the supplied Limits for subsequent require_runtime calls. Repeated shared signature/parameter/capture scans, capability searches, native signature comparisons and registry string comparisons charge their upper work bound before the comparison or inner traversal. Opcode support is normalized once to a bounded lookup table. An error's source-map lookup uses the previously verified PC ordering and binary search.

Symbol uniqueness uses transient standard-library hashes and a reserved sorted index. Equal-hash groups undergo exact byte sorting/equality; even a forced all-collision group avoids all-pairs comparison. Generic u32 uniqueness checks likewise use a reserved sorted index. These sorts reuse Rust 1.96 BinaryHeap and its existing Vec allocation: each push is charged for at most floor(log2(len)) upward levels; in-place heapsort is charged for two comparisons per downward level before it runs. String fallback comparison bounds include the group's maximum byte length. Retained Vec capacities and simultaneous collision-group scratch are checked before reservation, measured, and released explicitly. Hashes never enter the wire format.

`InstructionSpec::visit_registers_budgeted` charges known window/list sizes before iteration and can reject scalar visits; `visit_registers` is the unmetered adapter for trusted analysis. Match outputs use ConditionalWrite. CellNew has exactly one operand, BindingId: its Cell storage register comes from BindingDef. Explicit cells start uninitialized and retain the fixed type/mutable contract through every alias. The runtime retains the TypeId's code/content version and definition ModuleInstance; GC release state is separate from initialization.

Parameter(i) accepts Value(Reg(i)) and Cell(Reg(i)), with exact signature name/type/mutability and a unique record per parameter (including rest). Missing records retain implicit Value storage. Each script owner/default prefix must have identical storage, and mutable prefixes require Cell. The binder initializes each invocation's parameters in order; only its internal default-prefix path forwards Cell identity. Ordinary calls to a default FunctionDef create fresh bindings. CellNew remains restricted to Explicit cells. No fields, tags, opcodes or wire versions changed.

The transient parameter index stores only declared bindings and one function-offset directory. Binding scans, tuple sorting, prefix traversal and binary searches are charged before work, with fallible reserved Vec capacity charged before allocation. Tuple sorting reuses BinaryHeap's Floyd construction and in-place sort; searches charge ceil(log2(len))+1, including the final comparison for non-power-of-two lengths. Allocator tests check the simultaneous index capacity and reject one-byte-short scratch before reservation. A 20-function/1000-parameter shared-signature package without binding records avoids a dense 20,000-slot map; a separate 100-owner/100-default-prefix package decodes under max_work=300,000 but is rejected during the repeated prefix validation.

NativeDef reuses Signature without a new wire table. The last default's longest prefix defines canonical native storage: every related thunk agrees, and mutable prefixes require Cell just as script owners do. Readonly prefixes can consistently choose Value or Cell; parameters outside all prefixes and rest remain Value. The verifier normalizes shared native signatures once, validates all prefixes before publishing, and retains only sorted (signature, parameter) Cell pairs. `VerifiedProgram::native_parameter_storage(native,index)` returns BindingStorage with Reg(index); `native_parameters_need_cells(native)` selects the ordinary native no-Cell path. These immutable queries allocate nothing; V03 accounts its execution query work. `PackageStats::derived_capacity_bytes` reports cache capacity already included in owned_capacity_bytes. Runtime admission automatically counts it, while encode's dropped validation cache is excluded from its output-buffer ledger. No thunk rescanning is needed per invocation.

`exception_handler(function, original_unwind_pc, consumed_region)` treats the cursor as a consumed lexical subtree and only returns strict ancestors. Two finally layers unwind inner → outer → completion. Rethrow starts its unwind at the rethrow instruction while retaining the original error source. Normal handler entry and cleanup bypass (including fallthrough) are rejected; nested Leave that remains within an enclosing finally is valid. EndFinally requires its matching active runtime unwind record.

`cleanup_handler(function, original_pc, CleanupDestination::{Return, Leave(target)}, consumed_region)` queries the next Finally for a normal exit and returns the same HandlerTarget (exception=None). Runtime keeps original_pc and destination fixed, then advances consumed_region after each EndFinally. It shares the exception selector's exact all-block strict ancestor relation and compares the containing protected/catch block widths, including nested finally layers inside an outer catch whose protected range is shorter. Leave preserves an enclosing source block containing its destination; an already active finally is never selected again. Invalid function/source/destination/cursor indices are rejected. Queries borrow verified metadata, scan O(region_count), and allocate nothing; V03 owns saved unwind records and execution work accounting. Return cleanup has a verified three-layer catch nesting regression (inner → middle → outer → None), and Leave has target-sensitive cleanup and enclosing-catch/finally preservation checks. Exception routing continues through exception_handler.

## Reproducible checks

Executed 2026-10-08 UTC, rustc 1.96.0 (`ac68faa20`, LLVM 22.1.2), x86_64-unknown-linux-gnu. All commands below passed for the corrected implementation.

```sh
cargo fmt --all -- --check
cargo clippy -p aside-vm --all-targets -- -D warnings
cargo test -p aside-vm
cargo test -p aside-vm --release
cargo check -p aside-vm --all-targets --target i686-unknown-linux-gnu
cargo build -p aside-vm --lib --release --target i686-unknown-linux-gnu
cargo test -p aside-vm bytecode:: -- --nocapture
```

This environment installed the official pinned toolchain at `/workspace/toolchains`; its exact configuration was:

```sh
export RUSTUP_HOME=/workspace/toolchains/rustup
export CARGO_HOME=/workspace/toolchains/cargo
export PATH=/workspace/toolchains/cargo/bin:$PATH
export CARGO_TARGET_DIR=/tmp/reel-b01-target
```

The i686 commands used `/tmp/reel-b01-i686` as CARGO_TARGET_DIR. These are actual cross compilation and rlib build checks, not 32-bit execution results.

Debug and release each passed 54 unit tests (51 B01 plus 3 existing storage internals) and 15 storage integration tests. The behavior evidence includes every truncation position of all twelve snapshots (with rewritten total length to reach inner fields), scalar/type/descriptor round trips, all opcode families, unknown versions/tags/flags and unused operands, malformed indices/windows/signatures/captures/regions, UTF-8 boundaries, iterative type/pattern graph handling, and deterministic field mutations that cannot panic or publish invalid programs. The original eleven binary snapshots are unchanged by the native-layout addition.

The five independently reported regressions have explicit tests: a <2048-byte real encoded package declaring a billion-register window is rejected with max_work=20,000 before window traversal; the budgeted visitor observes no window registers before failed precharge; two consumed finally layers never re-enter the inner layer; split lexical parent blocks are rejected; valid nested Leave inside finally is accepted; erasing package/native capability declarations cannot bypass trusted host requirements. Scratch allocation/fill, bulk pattern copies and metadata scans are also charged before their inner work.

`shared_defaults_cannot_hide_function_parameter_capture_product_work` constructs 100 ordinary functions sharing a 100-default signature, 100 Plain/NoWait default thunks with matching preceding-parameter signatures, and 1000 captures per function. The real encoded package is 269,562 bytes with 205,757 records (within the default 262,144 record bound); complete verification including parameter-layout checks charges 11,882,820 work units. Byte decoding succeeds under max_work=1,000,000, then package validation rejects at function 5 before further shared capture comparisons. The recorded debug run rejected in approximately 53.81ms. Five additional regressions cover shared function parameters without defaults, shared native parameters, signature capability searches, positional-only call descriptors, and runtime registry admission. These checks do not change the wire format or existing golden snapshots.

## Ordinary project loading

`ordinary_project_loading_scales_under_default_limits` builds real packages with unique short function names, immutable linked globals, one register per function, and LoadConst(null)/Return bodies. The measurement includes two-pass decoding, semantic verification and runtime admission against all 77 supported opcodes; construction and encoding are outside the timed region. The following single-run release measurements used the same package constructor on original head `8121429` (isolated temporary checkout) and the optimized implementation, with unchanged default limits. A Limit result measures rejection time, not successful loading.

| Functions | Symbols | Encoded bytes | Original load | Optimized load | Optimized verification work | Verification scratch peak |
|---:|---:|---:|---|---|---:|---:|
| 1,000 | 1,002 | 121,173 | OK, 4.34ms | OK, 0.94ms | 235,329 | 16,032 |
| 5,000 | 5,002 | 605,173 | Limit, 19.53ms | OK, 5.68ms | 1,300,870 | 80,032 |
| 10,000 | 10,002 | 1,210,173 | Limit, 21.49ms | OK, 10.25ms | 2,691,309 | 160,032 |

The 10k package retains 2,490,535 bytes including inline package data and 60,008 records. The regression also supplies 10,000 repeated supported-opcode entries and verifies normalization once, unchanged canonical re-encoding, and default-limit success at all three scales. `symbol_hash_collisions_keep_exact_unique_checks_and_bounded_scratch` forces 10,000 strings into one hash group, checks exact duplicate rejection and low-work rejection, and compares actual allocator peak to accounted scratch. A separate test rejects collision scratch before reservation when the allocation budget is one byte short. These are x86_64 measurements, with no low-end device execution claim.

## Byte and capacity measurements

`decoder_capacity_accounting_matches_actual_allocator_residency` uses allocation-counter 0.8.1 to compare live allocator bytes to every retained Vec/String capacity, rather than counting elements alone. Results below are 64-bit decoder heap measurements, excluding the caller's borrowed encoded input. Each package has one module: its temporary count directory explains the four-byte difference between heap residency and peak. PackageStats additionally includes 240 bytes of inline BytecodePackage data and retained derived cache capacity; it separately reports verification scratch peak. The native-default snapshot retains 16 extra cache bytes: VerifiedProgram's total owned_capacity_bytes is 3014, and actual live allocator bytes are 2774. A dedicated test matches this entire verified retention. Scratch tests match the simultaneous parameter index + normalized signature directory + Cell cache and reject limits before allocation/traversal. Callers account their input buffer and execution/VerifiedProgram wrapper metadata in their own ledger. No runtime frame slots are allocated by B01.

| Shared snapshot | Encoded bytes | Retained heap | Decode peak | Verification scratch peak |
|---|---:|---:|---:|---:|
| helper-native | 748 | 1605 | 1609 | 224 |
| cell-closures | 753 | 1490 | 1494 | 224 |
| exception-regions | 607 | 1177 | 1181 | 224 |
| captured-default | 798 | 1759 | 1763 | 224 |
| duplicate-arguments | 811 | 1717 | 1721 | 224 |
| forward-readonly-cell | 687 | 1482 | 1486 | 224 |
| initialized-null-cell | 687 | 1482 | 1486 | 224 |
| uninitialized-cell-read | 722 | 1562 | 1566 | 224 |
| second-cell-initialization | 703 | 1498 | 1502 | 224 |
| readonly-cell-write | 703 | 1498 | 1502 | 224 |
| parameter-cell-default | 1226 | 2621 | 2625 | 256 |
| native-parameter-defaults | 1275 | 2758 | 2762 | 336 |

Bad header/count and instruction-index probes allocate less than 2048 bytes total (small bounded metadata/error records) and no decoded payloads. Input byte limits, aggregate record/capacity limits and checked arithmetic precede payload reservation; the scan pass materializes no vectors/strings except the bounded module-count directory. Semantic conflicts are rejected before VerifiedProgram publication.

## Runtime acceptance handoff

`fixtures::helper_native()` is the same package for both ECHO_COMPLETIONS (Ready and Pending): main → ordinary helper → echo, argument 41, result 42, one native entry, one return destination. The dispatcher must test budget=1, repeated/old completion rejection, GC during suspension and result lifetimes. B01 has verified/decoded/disassembled these packages; it has not executed their language semantics.

`fixtures::CELL_CASES` distinguishes forward readonly capture (read 1 after BindInit), initialized null, reading before initialization, second initialization and readonly writes. Capture copies the cell handle without reading its contents. The runtime must implement the initialization bit and fixed type/write contract, publish through the barrier, and route or reject ordinary set_field(Cell) through the same checked entry. The current storage Cell's GC-release Option must not serve as that language state.

Default-expression capture layouts and module identity are shared with the callee closure and preceding parameters are explicit inputs. Class method/initializer and pattern-default captures also have mappings. Duplicate argument entries remain ordered until the final binder, after all actual argument side effects. Native CallScript/Continue/Ready/Pending execution, semantic type/effect checks and runtime unwinding remain the following task's responsibility.

`fixtures::parameter_cell_default()` starts with x=41. Its owner has mutable x in Parameter(0)/Cell(Reg(0)); y's default function shares that prefix, increments x and returns a closure capturing its cell. The owner's body increments x again. Main also calls the same default FunctionDef normally with the original author value 41. The V03 semantic expectation is an owner pair (43, closure reading 43) and an ordinary-call closure reading 42 from its independent invocation cell. B01 verifies the package, rejects owner/default name/type/mutable/storage/register/duplicate-binding/module conflicts, tests matching readonly Value/Cell prefixes and rest Cells, and prints the bindings. It has not executed that expectation or performed V03 independent acceptance.

`fixtures::native_parameter_defaults()` declares native x/y/z/rest. Its two Plain/NoWait defaults share mutable x in Cell; the second captures readonly y in Cell and calls that nested closure. Derived storage is Cell/Cell/Value/Value. V03's expected author arguments after sequential defaults are x=43, y=42, z=42, rest=[]; tickets expose author values and keep Cell identities internal. B01 verifies/roundtrips/disassembles this package, rejects mutable Value prefixes and inconsistent readonly prefixes in either direction, checks names/types/mutable/slots/duplicate records, and confirms 100 native declarations sharing the signature retain one 16-byte cache. V03's current native-script-default load rejection requires its own implementation coordination; validation success does not establish execution support.
