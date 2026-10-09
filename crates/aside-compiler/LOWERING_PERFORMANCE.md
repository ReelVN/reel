# L04 compiler validation and measurements

Historical observations below describe their original revisions. Raw evidence links
point to the pre-cleanup Git revision; new generated output belongs under
`target/reel-artifacts/`. Current maintenance commands are documented in
[`tools/reel-dev`](../../tools/reel-dev/README.md).

Implementation measured: `2edce46ca236c6424db61d8f3dd89b0e5fd19b25`.
Rust 1.96.0, Linux x86_64/glibc 2.41, 2026-10-08, release optimization.
The initial dataset remains pinned to that revision. The 2026-10-09 source-review
corrections and their separate measurements are recorded below.
Each accepted workload ran seven fresh processes; times below are medians.
These are local compiler measurements, with no earlier lowering baseline or
runtime throughput claim. Container scheduling can affect elapsed times.

## Recorded results

`lower` builds IR and shared tables; `emit + verify` relocates code and runs B01
verification. Parsing, L03 analysis and bytecode codec times are separately
recorded in the raw data. Work counts are prepaid compiler operations; B01
verification work has its own counter. Retained bytes cover new Vec/String
capacities plus verifier-derived metadata after scratch has been released.
Every one of the 91 accepted samples asserts equality with the actual retained
allocation from `stats_alloc`. Borrowed analyzed input is excluded. Peak bytes
are a conservative capacity upper bound including replacement allocations and
verification scratch; they are not measured RSS or an allocator peak.

| Workload | Size | Lower ms | Emit + verify ms | Retained bytes | Peak upper bytes | Compiler work | Instructions | Max registers/function |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Plain functions | 1,000 | 1.056 | 0.766 | 710,800 | 919,476 | 529,784 | 5,002 | 1 |
| Plain functions | 10,000 | 12.649 | 8.100 | 9,632,720 | 13,023,224 | 6,667,875 | 50,002 | 1 |
| Mutable captured closures | 1,000 | 3.371 | 2.799 | 2,211,962 | 2,409,022 | 2,003,597 | 28,002 | 4 |
| Mutable captured closures | 2,000 | 6.592 | 5.779 | 4,424,098 | 4,817,766 | 4,138,284 | 56,002 | 4 |
| Reversed named arguments | 1,000 | 0.760 | 0.502 | 280,396 | 379,152 | 471,623 | 4,014 | 1,002 |
| Reversed named arguments | 10,000 | 10.795 | 6.728 | 4,468,876 | 6,042,192 | 6,553,806 | 40,014 | 10,002 |
| Sequential temporary expressions | 1,000 | 0.324 | 0.454 | 333,097 | 529,965 | 88,539 | 6,007 | 3 |
| Sequential temporary expressions | 10,000 | 3.031 | 4.373 | 3,212,585 | 4,785,709 | 863,795 | 60,007 | 3 |

The 10,000-expression function reuses three temporary registers. Named calls
retain the complete contiguous actual-argument window until the Call; their
register count grows with simultaneously live arguments, as required. Plain and
capture workloads have separate functions, so aggregate register totals grow
while their per-function maximum stays bounded.

Three larger workloads were rejected normally, rather than reported successful:

| Workload | Size | Stage | Diagnostic | Source byte range | Compiler work charged |
| --- | ---: | --- | --- | --- | ---: |
| Mutable captured closures | 5,000 | Shared verification | B01 capacity/record limit | 0..253890 | 10,778,201 |
| Sequential temporary expressions | 25,000 | Shared verification | B01 capacity/record limit | 0..100009 | 2,486,299 |
| Sequential temporary expressions | 50,000 | IR construction | Compiler instruction limit | 133335..133338 | 2,649,064 |

Default compiler and B01 quotas are independent. The first two exceed B01's
shared record admission; the third reaches the compiler's 200,000-instruction
budget before constructing further operations. Located errors are returned and
no bytecode is executed.

## Reproduction and correctness checks

Run from the repository root with the pinned toolchain on PATH:

```sh
cargo build -p aside-compiler --example lowering_probe --release --locked
cargo run --locked -p reel-dev -- compiler-probe lowering
```

The runner preserves individual samples, medians, environment/revision and
rejections in [bench-results/lowering](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/aside-compiler/bench-results/lowering). The source
generator and retained-allocation assertion are in
[examples/lowering_probe.rs](examples/lowering_probe.rs).

Initial 2026-10-08 implementation checks passed in debug and release:

```sh
cargo fmt --all --check
cargo clippy -p aside-compiler --all-targets --locked -- -D warnings
cargo test -p aside-compiler --all-targets --locked
cargo test -p aside-compiler --all-targets --release --locked
cargo run --locked -p reel-dev -- check
git diff --check
```

The compiler suites contain 97 tests, including 19 lowering contracts. Tests
compile source with L03, inspect shared instruction register reads/writes and
control-flow liveness, check callee/argument source order and saved operands
across nested calls, distinguish uninitialized bindings from null, compare
capture/parameter identities, preserve source Context and trusted native
registration/namespace provenance, and verify/encode/decode/disassemble with
B01. Every emitted PC maps to a valid original UTF-8 byte range. Regression
coverage includes value subexpressions whose branches return and the remaining
short-circuit fallthrough edge. Quota tests cover work, capacity, records,
instructions, registers, blocks, depth, source admission and shared emission.
Fixture integrity reports 10 immutable T01 sources/maps and 32 unchanged
assertion mappings; execution remains `waiting_runtime`.

## 2026-10-09 source-review corrections

The branch preserves the real merge of L03
`4500f788c5cd56bd431d6f75424f0c8a756a540d`; it does not copy a second semantic
implementation. L03's author reported 83 compiler tests in debug/release and
fmt/strict clippy. This L04 environment reran the combined branch after the while
and module-binding fixes: **106 compiler tests passed in each of debug and
release**, including 22 lowering contracts and the five L03 default-control
tests. These are the task author's combined checks, not the integration owner's
independent acceptance. fmt, strict clippy, fixture integrity and diff checks
also passed. The language file exactly matches the owner's
`65f8ba9177ab81e817202fb56fdd92bab10fe6d0` baseline; both creator guides explain
the same default rule without changing their 24 matched source scenes.

Static source regressions cover labelled/unlabelled while condition transfers,
conditions ending in break/continue or return, transfers to an outer loop,
target retirement, omitted unreachable body calls and a condition Leave selecting
the shared finally handler. Module-loop/block/condition/catch declarations and
ownerless block-fragment locals fail at their declaration with zero compiler
capacity admitted; root module globals and ordinary function-local shadowing
still compile. InvalidDefault is now an L03 diagnostic, before AnalyzedModule
admission. Default block tails, nested author returns, internal loops and throw
compile through B01 verification/codec/disassembly. The compiler-generated Return
still ends the default thunk; it is distinct from a rejected source return.

Loop measurements use production revision
`f1040372d28a0a3a82e7c3162926321d804db4d0` and the same Rust/Linux environment.
The generator repeats `while choose(if flag{break;}else{continue;}){}` in one
function; choose is only compiled and never executed. Each accepted size has
seven fresh-process samples, 28 in total. Every sample asserts exact retained
allocation equality. These are medians; peak remains a conservative capacity
upper bound, not RSS.

| Loops | Lower ms | Emit + verify ms | Retained bytes | Peak upper bytes | Compiler work | Max registers/function |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 100 | 0.066 | 0.057 | 51,706 | 118,354 | 44,446 | 5 |
| 1,000 | 0.532 | 0.510 | 395,770 | 935,698 | 401,274 | 5 |
| 5,000 | 2.453 | 2.234 | 1,837,562 | 4,175,122 | 1,937,114 | 5 |
| 10,000 | 7.864 | 4.593 | 3,672,570 | 8,347,282 | 3,872,786 | 5 |

At 20,000 loops, lowering normally rejects at the 100,000-block quota: source
bytes `783337..783384`, 6,266,280 charged compiler work units. The normal rejection
is recorded rather than counted as a successful compilation. Raw samples,
medians, environment and the rejection are the `2026-10-09*` files in
[bench-results/lowering](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/aside-compiler/bench-results/lowering). Reproduce from the root:

```sh
cargo build -p aside-compiler --example lowering_probe --release --locked
cargo run --locked -p reel-dev -- compiler-probe lowering loops
```

## Pending acceptance

No VM dispatch or native host invocation was run. V03 independent execution
validation remains pending its platform blocker; this task does not reroute it.
The source default-return rule is resolved by the owner baseline and L03; it is
no longer a missing transfer contract. Current public analysis cannot produce a
combined Init+Derived declaration. Cumulative restrictions on actual call chains
belong to VM flags, whose execution has not been verified by this task.
Initializer-local/per-entry Cell lifetime and full-language deferred scopes are
listed in [LOWERING.md](LOWERING.md); unsupported module bindings are diagnosed,
not silently initialized as globals. Integration-owner review and dependency
acceptance remain separate from these compiler results.
