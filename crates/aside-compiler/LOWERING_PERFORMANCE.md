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

## 2026-10-09 initializer-local compilation (L05)

Production, tests and probe: [81bef3405f0df4f248724e6a636f0f5b4e759fc4](https://github.com/jhq223/reel/blob/81bef3405f0df4f248724e6a636f0f5b4e759fc4/crates/aside-compiler/examples/lowering_probe.rs),
based on main `9e0e10b856a1a6995da0a9268b605cd508e3ad71`, Issue #37.
Rust 1.96.0 (ac68faa20), Linux 6.18.44 x86_64, release optimization.
These are the task author's static results; integration review is separate.

The pinned base passed 108 compiler tests in debug and release. L05 passed
119 in each mode, including 29 lowering contracts. Both original module-local
rejection tests retain their source cases and now assert initializer identity,
reset ordering and successful B01 roundtrips; NoWait-only fragments retain their
rejections. The 11 added contracts cover shared origin/owner, while condition
reentry, shadowing/exports, sibling/transitive/default captures, forward read-only
and recursion, Cell promotion of prior users, catch/finally CFG edges, enclosing
analysis plus pre-admission link refusal, non-root exports and quota boundaries.
Capture checks compare complete layouts and every Binding/Capture source against
HIR SymbolId metadata. A conservative shared CFG checks reset dominance, and a
named-function scope checks all CellNew/Clear before its first MakeClosure.
The existing 24 paired author scenes passed parse and semantic checks unchanged.

Validation commands, limited to the compiler (no workspace/VM execution tests):

```sh
cargo test -p aside-compiler --lib --tests --locked
cargo test -p aside-compiler --lib --tests --release --locked
cargo fmt -p aside-compiler --check
cargo clippy -p aside-compiler --all-targets --locked -- -D warnings
git diff --check
cargo build -p aside-compiler --example lowering_probe --release --locked
```

fmt, strict clippy and diff checks passed. The temporary environment initially
had no Rust installation; the exact pinned toolchain was installed under /tmp
without changing repository/toolchain configuration. A brief execution-server
disconnect was recovered and subsequent commands actually completed; it did not
require a different execution entry or loosened verification restrictions.

The `initializer` probe compiles one root global, while-condition locals/capture,
and N body blocks with mutable local x and two closures. It parses/analyzes/lowers,
verifies and roundtrips the emitted package; none of those closures is invoked.
Each accepted N used three fresh-process samples; the table gives medians.
All nine accepted samples assert exact equality between retained allocations
and the compiler capacity ledger. HIR/input storage is excluded from this ledger;
Symbol.origin is included in semantic size_of-based retained-byte statistics.
Peak remains a conservative capacity bound rather than measured RSS.

| Body blocks | Lower ms | Emit + verify ms | Retained bytes | Peak upper bytes | Compiler work | Instructions | Max registers/function |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 100 | 0.406 | 0.325 | 361,270 | 500,754 | 307,113 | 4,746 | 306 |
| 500 | 1.952 | 1.648 | 1,646,806 | 2,281,522 | 1,602,290 | 23,546 | 1,506 |
| 1,000 | 3.814 | 3.341 | 3,294,430 | 4,562,522 | 3,276,195 | 47,046 | 3,006 |

Fixed slots grow with source binding count under the existing pinned-slot model,
not with the number of dynamic loop iterations. No runtime allocation rate or
Cell/GC lifetime is measured. At 5,000 body blocks the compiler returns a normal
instruction-quota rejection while lowering: source bytes `266543..266549`,
14,993,380 charged work units. Register-limit tests also reject before allocating
BindingDef/local tables at the failing declaration, while exact semantic work,
symbol/capture and compiler capacity limits have successful admission cases.

Reproduce each accepted command three times and the rejection once; keep output
under ignored `target/reel-artifacts/issue37-baseline/`:

```sh
target/release/examples/lowering_probe 100 initializer
target/release/examples/lowering_probe 500 initializer
target/release/examples/lowering_probe 1000 initializer
target/release/examples/lowering_probe 5000 initializer
```

The worktree's raw files are `initializer-N-SAMPLE.log` and
`initializer-summary.json`; debug/release/clippy logs use the `final-` prefix.
Generated output is not committed. These static results cannot establish actual
closure sharing/reentry isolation, GC or host restoration, module publication,
complete script imports or a playable engine session. V03 and every Aside source
execution remain untested and were not invoked through alternative entry points.

## 2026-10-09 fixed collections and index locations (L06)

Production, tests and probe: `9ff3e67ade4797e9dd348d5cb038c258daeffa2b`, based
on main `6203ee138242baa74847f41c66e92382147aa5b1`, Issue #40. Rust 1.96.0
(`ac68faa20`), Linux 6.18.44 x86_64, release optimization. Changes are confined to
the existing aside-compiler crate; B01/VM/bytecode v1/RuntimeSupport are unchanged.
These are the implementer's static checks, pending the owner's independent review.

At this production revision, all **134 compiler tests** passed in both debug and
release, including 44 lowering contracts and 15 L06 contracts. The existing 24
paired author-guide scenes passed parse/semantic checks. fmt, strict clippy and
diff checks passed. Final check logs are under ignored
`target/reel-artifacts/issue40/final-{debug,release,fmt,clippy,probe-build}.log`.
The final full checks include the Name-location budget-span correction. The
probe generator's initial invalid `catch(e)` was corrected to `catch e` before
measurements; an initial expected refusal at 5,000 table fields actually compiled
and is retained as a successful larger boundary sample.

The [static matrix](tests/lowering/collections_locations.rs) proves ordered
constructor descriptors, saved receiver/key/element registers across calls,
reverse chain writes, all ten compound operations, default/capture identity and
normal temporary clears. Terminal receiver/key/element/RHS branches omit later
calls, constructors and writes; Return/Throw retain shared finally selection.
Conditional fallthrough has definite temporary initialization and conservative
shared-CFG dominance checks. Deferred forms and known tuple/immutable-name writes
remain located errors. Work/capacity tests reject just below measured budgets and
admit the exact lower budget; collection/location register and nesting quotas
reject at their failing source. The repeated 300-expression contract keeps at
most 12 registers instead of growing with expression count.

All 53 accepted probe processes assert **both IR retained capacity and final
verified-package retained capacity** against `stats_alloc`, excluding borrowed
input/HIR. This matters for ordered pair descriptors and temporary prepared
locations: emission's verifier ledger cannot mask an earlier IR ledger error.
There are 45 L06 size-series samples (three fresh processes per size), one larger
table boundary sample, six existing plain/captures/temporaries/named/loops/
initializer samples and one generated structural-evidence sample. No seed package
is used. Peak bytes remain a conservative capacity upper bound, not measured RSS.

`sequences` constructs N `(id(i), [id(i)])` entries in a single outer array;
`tables` constructs N dynamic key/value fields with repeated keys; `locations`
repeats a mixed chain and index compound assignment N times. `nested` has N nested
arrays. `terminal` repeats a table whose key throws on both conditional branches,
inside catch/finally, to exercise abandoned pair/location scratch. These functions
and calls are compiled only. Times below are medians of three processes, except
the explicitly marked 5,000-field boundary sample, which ran once.

| Workload | N | Lower ms | Emit + verify ms | Retained bytes | Peak upper bytes | Compiler work | Instructions | Max registers/function | IR retained bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Sequences | 100 | 0.144 | 0.203 | 208,944 | 307,700 | 55,060 | 2,812 | 106 | 176,612 |
| Sequences | 500 | 0.638 | 1.029 | 837,680 | 1,231,348 | 261,428 | 14,012 | 506 | 707,044 |
| Sequences | 1,000 | 1.286 | 2.028 | 1,673,264 | 2,460,148 | 522,588 | 28,012 | 1,006 | 1,411,556 |
| Tables | 100 | 0.100 | 0.150 | 111,664 | 161,268 | 39,612 | 2,012 | 203 | 95,716 |
| Tables | 500 | 0.440 | 0.739 | 702,512 | 1,096,180 | 216,092 | 10,012 | 1,003 | 571,876 |
| Tables | 1,000 | 0.839 | 1.548 | 1,402,928 | 2,189,812 | 431,476 | 20,012 | 2,003 | 1,141,220 |
| Tables (one boundary sample) | 5,000 | 4.550 | 7.272 | 7,014,448 | 10,160,628 | 2,120,564 | 100,012 | 10,003 | 5,966,308 |
| Locations | 100 | 0.617 | 0.423 | 354,661 | 551,721 | 150,108 | 5,416 | 9 | 289,561 |
| Locations | 500 | 1.773 | 2.348 | 1,739,109 | 2,525,993 | 720,492 | 27,016 | 9 | 1,477,401 |
| Locations | 1,000 | 3.295 | 4.513 | 3,475,813 | 5,049,129 | 1,441,410 | 54,016 | 9 | 2,951,961 |
| Nested | 10 | 0.012 | 0.014 | 4,489 | 6,285 | 1,184 | 47 | 11 | 4,113 |
| Nested | 40 | 0.021 | 0.021 | 13,801 | 20,205 | 2,984 | 167 | 41 | 11,889 |
| Nested | 80 | 0.028 | 0.032 | 26,217 | 38,765 | 5,384 | 327 | 81 | 22,257 |
| Terminal | 10 | 0.064 | 0.060 | 24,367 | 39,463 | 11,698 | 267 | 20 | 21,691 |
| Terminal | 100 | 0.238 | 2.290 | 184,127 | 321,879 | 102,552 | 2,607 | 110 | 178,411 |
| Terminal | 300 | 0.745 | 18.710 | 468,863 | 930,839 | 309,328 | 7,807 | 310 | 627,627 |

Simultaneously live fixed collection elements grow the register window; sequential
index operations reuse nine registers. Terminal register growth includes N pinned
catch bindings under the existing source-binding slot model. Increasing shared
exception-region verification cost in the terminal workload is visible here; it
is not an execution/finally throughput claim or a new verifier optimization.

Four probes return normal located lower errors (process exit 0, `rejected` output):

| Workload | N | Diagnostic | Source byte range | Charged compiler work |
| --- | ---: | --- | --- | ---: |
| Sequences | 10,000 | Instruction quota | 160757..160761 | 3,387,177 |
| Tables | 33,000 | Register window quota, before pair allocation/element lowering | 19..582911 | 2,457,508 |
| Locations | 5,000 | Instruction quota | 216672..216683 | 5,197,403 |
| Nested, lower depth set to 64 | 80 | Lower depth quota | 69..106 | 1,519 |

Reproduction uses only the compiler:

```sh
cargo test -p aside-compiler --lib --tests --locked
cargo test -p aside-compiler --lib --tests --release --locked
cargo fmt -p aside-compiler --check
cargo clippy -p aside-compiler --all-targets --locked -- -D warnings
cargo test -p aside-compiler --test lowering collections_locations --locked -- --nocapture
cargo build -p aside-compiler --example lowering_probe --release --locked
target/release/examples/lowering_probe 100 sequences
target/release/examples/lowering_probe 500 sequences
target/release/examples/lowering_probe 1000 sequences
target/release/examples/lowering_probe 100 tables
target/release/examples/lowering_probe 500 tables
target/release/examples/lowering_probe 1000 tables
target/release/examples/lowering_probe 100 locations
target/release/examples/lowering_probe 500 locations
target/release/examples/lowering_probe 1000 locations
target/release/examples/lowering_probe 10 nested
target/release/examples/lowering_probe 40 nested
target/release/examples/lowering_probe 80 nested
target/release/examples/lowering_probe 10 terminal
target/release/examples/lowering_probe 100 terminal
target/release/examples/lowering_probe 300 terminal
target/release/examples/lowering_probe 5000 tables
target/release/examples/lowering_probe 10000 sequences
target/release/examples/lowering_probe 33000 tables
target/release/examples/lowering_probe 5000 locations
target/release/examples/lowering_probe 80 nested 64
ASIDE_LOWERING_EVIDENCE=target/reel-artifacts/issue40/evidence target/release/examples/lowering_probe 1 evidence
git diff --check
```

Run each size-series command three times. Raw `MODE-N-SAMPLE.log`, four
`MODE-N-rejection.log`, `tables-5000-boundary.log` and `measurements.json` remain in
ignored `target/reel-artifacts/issue40/`. The evidence directory contains actual
source, IR, ordered descriptor tables, codec output and disassembly plus a SHA-256
manifest tied to the production head. `static-matrix.log` records, for example,
calls `obj(a), key(0), obj(b), key(1), rhs()` followed by IndexSet for
`obj(b)[key(1)]`, IndexSet for `obj(a)[key(0)]` and BindWrite for `n`.

These checks prove compilation order and register/CFG structure. They do not
execute or simulate any Aside bytecode, establish actual key hash/duplicate
overwrite behavior, index bounds, alias mutation, GC/restoration or a complete
engine session. Source/index/bilingual-guide synchronization and independent
integration review belong to the owner; the PR remains draft until those steps.

## Pending acceptance

No VM dispatch or native host invocation was run. V03 independent execution
validation remains pending its platform blocker; this task does not reroute it.
The source default-return rule is resolved by the owner baseline and L03; it is
no longer a missing transfer contract. Current public analysis cannot produce a
combined Init+Derived declaration. Cumulative restrictions on actual call chains
belong to VM flags, whose execution has not been verified by this task.
L05 initializer-local compilation and its structural Cell checks are recorded
above; actual Cell execution/lifetime acceptance remains pending. Full-language
deferred scopes and external-environment link boundaries are listed in
[LOWERING.md](LOWERING.md). The owner maintains language/index/guide status;
integration-owner source review and acceptance remain separate from this report.

## 2026-10-09 nonoptional members (L07)

Implementation [c428a329](https://github.com/jhq223/reel/commit/c428a3293258d82cc93d3d22bd43d3d39cf61c05),
Issue #42 / PR #43, builds on main fd12d144. The author's Rust 1.96.0
static checks report 146 compiler tests per debug/release profile (56 lowering,
including 12 Member tests), and 51 B01 tests per profile. fmt and compiler
all-target clippy with warnings denied passed. Independent acceptance is
recorded separately in the PR.

The existing lowering_probe now has a members mode. Its source repeats a mixed
Name/Member/Index chain with a receiver-rebinding RHS, Member compound assignment
and dynamic member call. Fresh processes at N=1 and N=1000 passed the retained
allocation checks; the maximum function register count was 9 in both.
At N=1000 the author measured 66,027 instructions and 5,409,572 retained bytes,
equal to the actual retained allocation counter. This is a capacity/structure
measurement, not a speedup comparison or runtime execution result.

Reproduce the static workload:

```sh
cargo build -p aside-compiler --example lowering_probe --release --locked
target/release/examples/lowering_probe 1 members
target/release/examples/lowering_probe 1000 members
cargo test -p aside-compiler --test lowering members --locked
cargo test -p aside-compiler --test lowering members --release --locked
```

No V03, source execution or bytecode interpretation is involved. Actual member
binding, setter/metamethod effects, GC and restoration remain outside these
measurements. Generated evidence belongs under ignored target/reel-artifacts.
