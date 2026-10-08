# B01 shared bytecode validation evidence

Implemented in `aside-vm::bytecode`; format contract and all 77 opcode entries are in `FORMAT.md` and `spec.rs`. This is program data, verification and disassembly. It contains no dispatcher, frontend dependency, heap mutation or GC/root-range code.

The branch combines accepted D01 `c03226a` and storage `95a7e84`. Owner language clarifications `999a8e3` and `c18db94` are preserved. The initial interface checkpoint was `178df78`; `fb3c1fc23f5f34c5a73b02af967bbc97f296b007` closed the initial five reported regressions. The follow-up shared-graph work accounting and its evidence are described below.

## Loading and integration

Tooling calls `decode(bytes, limits)` for bounded shape/index decoding, then `verify(package, limits)` (or `VerifiedProgram::decode`) for semantic package validation. `disassemble(&program, &mut sink)` writes a listing including descriptors, regions, native versions and source byte spans. The validated package is immutable through the public API; `VerifiedProgram` construction is private.

Execution loading uses `VerifiedProgram::load(bytes, limits, &support)`. The trusted native adapter independently resolves its registration's parameter names/types into the package tables, then supplies the exact signature and `required_capabilities`. Every trusted native requirement must be authorized and declared in both the package and native record. Copying untrusted declarations into RuntimeSupport is not registration validation. Unsupported opcodes are rejected even when unreachable.

Decoding, package verification and runtime admission each enforce max_work independently. VerifiedProgram retains the supplied Limits for subsequent require_runtime calls. Repeated shared signature/parameter/capture scans, capability searches, native signature comparisons, registry string comparisons and opcode support scans charge their upper work bound before the comparison or inner traversal. An error's source-map lookup uses the previously verified PC ordering and binary search.

`InstructionSpec::visit_registers_budgeted` charges known window/list sizes before iteration and can reject scalar visits; `visit_registers` is the unmetered adapter for trusted analysis. Match outputs use ConditionalWrite. CellNew has exactly one operand, BindingId: its Cell storage register comes from BindingDef. Explicit cells start uninitialized and retain the fixed type/mutable contract through every alias. The runtime retains the TypeId's code/content version and definition ModuleInstance; GC release state is separate from initialization.

`exception_handler(function, original_unwind_pc, consumed_region)` treats the cursor as a consumed lexical subtree and only returns strict ancestors. Two finally layers unwind inner → outer → completion. Rethrow starts its unwind at the rethrow instruction while retaining the original error source. Normal handler entry and cleanup bypass (including fallthrough) are rejected; nested Leave that remains within an enclosing finally is valid. EndFinally requires its matching active runtime unwind record.

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

Debug and release each passed 34 unit tests (31 B01 plus 3 existing storage internals) and 15 storage integration tests. The behavior evidence includes every truncation position of all ten snapshots (with rewritten total length to reach inner fields), scalar/type/descriptor round trips, all opcode families, unknown versions/tags/flags and unused operands, malformed indices/windows/signatures/captures/regions, UTF-8 boundaries, iterative type/pattern graph handling, and deterministic field mutations that cannot panic or publish invalid programs.

The five independently reported regressions have explicit tests: a <2048-byte real encoded package declaring a billion-register window is rejected with max_work=20,000 before window traversal; the budgeted visitor observes no window registers before failed precharge; two consumed finally layers never re-enter the inner layer; split lexical parent blocks are rejected; valid nested Leave inside finally is accepted; erasing package/native capability declarations cannot bypass trusted host requirements. Scratch allocation/fill, bulk pattern copies and metadata scans are also charged before their inner work.

`shared_defaults_cannot_hide_function_parameter_capture_product_work` constructs 100 ordinary functions sharing a 100-default signature, 100 Plain/NoWait default thunks with matching preceding-parameter signatures, and 1000 captures per function. The real encoded package is 269,562 bytes with 205,757 records (within the default 262,144 record bound); complete verification charges 10,485,168 work units. Byte decoding succeeds under max_work=1,000,000, then package validation rejects at function 5 before further shared capture comparisons. The recorded debug run rejected in approximately 52ms. Five additional regressions cover shared function parameters without defaults, shared native parameters, signature capability searches, positional-only call descriptors, and runtime registry admission. These checks do not change the wire format or golden snapshots.

## Byte and capacity measurements

`decoder_capacity_accounting_matches_actual_allocator_residency` uses allocation-counter 0.8.1 to compare live allocator bytes to every retained Vec/String capacity, rather than counting elements alone. Results below are 64-bit decoder heap measurements, excluding the caller's borrowed encoded input. Each package has one module: its temporary count directory explains the four-byte difference between heap residency and peak. PackageStats additionally includes 240 bytes of inline BytecodePackage data; it separately reports verification scratch peak. Callers account their input buffer and execution/VerifiedProgram wrapper metadata in their own ledger. No runtime frame slots are allocated by B01.

| Shared snapshot | Encoded bytes | Retained heap | Decode peak | Verification scratch peak |
|---|---:|---:|---:|---:|
| helper-native | 748 | 1605 | 1609 | 68 |
| cell-closures | 753 | 1490 | 1494 | 51 |
| exception-regions | 607 | 1177 | 1181 | 51 |
| captured-default | 798 | 1759 | 1763 | 85 |
| duplicate-arguments | 811 | 1717 | 1721 | 68 |
| forward-readonly-cell | 687 | 1482 | 1486 | 68 |
| initialized-null-cell | 687 | 1482 | 1486 | 68 |
| uninitialized-cell-read | 722 | 1562 | 1566 | 68 |
| second-cell-initialization | 703 | 1498 | 1502 | 68 |
| readonly-cell-write | 703 | 1498 | 1502 | 68 |

Bad header/count and instruction-index probes allocate less than 2048 bytes total (small bounded metadata/error records) and no decoded payloads. Input byte limits, aggregate record/capacity limits and checked arithmetic precede payload reservation; the scan pass materializes no vectors/strings except the bounded module-count directory. Semantic conflicts are rejected before VerifiedProgram publication.

## Runtime acceptance handoff

`fixtures::helper_native()` is the same package for both ECHO_COMPLETIONS (Ready and Pending): main → ordinary helper → echo, argument 41, result 42, one native entry, one return destination. The dispatcher must test budget=1, repeated/old completion rejection, GC during suspension and result lifetimes. B01 has verified/decoded/disassembled these packages; it has not executed their language semantics.

`fixtures::CELL_CASES` distinguishes forward readonly capture (read 1 after BindInit), initialized null, reading before initialization, second initialization and readonly writes. Capture copies the cell handle without reading its contents. The runtime must implement the initialization bit and fixed type/write contract, publish through the barrier, and route or reject ordinary set_field(Cell) through the same checked entry. The current storage Cell's GC-release Option must not serve as that language state.

Default-expression capture layouts and module identity are shared with the callee closure and preceding parameters are explicit inputs. Class method/initializer and pattern-default captures also have mappings. Duplicate argument entries remain ordered until the final binder, after all actual argument side effects. Native CallScript/Continue/Ready/Pending execution, semantic type/effect checks and runtime unwinding remain the following task's responsibility.
