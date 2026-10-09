# L03 semantic analysis: measured source behavior and cost

Historical observations below describe their original revisions. Raw evidence links
point to the pre-cleanup Git revision; new generated output belongs under
`target/reel-artifacts/`. Current maintenance commands are documented in
[`tools/reel-dev`](../../tools/reel-dev/README.md).

Rust 1.96.0, Linux x86_64 KVM, AMD EPYC 9V74 (5 visible vCPUs), release.
This measures the compiler, not bytecode or script execution. It does not establish
full-engine, low-end-device, 32-bit, or VM memory results.

`semantic_probe` generates source before timing, runs the real parser, then
projects/analyzes the successful CST. Each row below is the median of seven
separate processes. Raw CSVs and environment metadata are in
[bench-results/semantic](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/aside-compiler/bench-results/semantic).

| Capture workload | Source bytes | HIR nodes | Parse ms | Analysis ms | Allocator-retained MiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1,000 functions | 70,890 | 24,001 | 3.23 | 9.55 | 5.72 |
| 5,000 functions | 358,890 | 120,001 | 15.96 | 51.27 | 31.34 |
| 10,000 functions | 718,890 | 240,001 | 31.97 | 104.75 | 62.68 |

Each function declares a typed/default parameter, mutable local, cell-capturing
closure, compound write, and call. The largest source produces 40,000 symbols,
10,000 captures and 190,000 charged analysis units. A HirNode occupies 72 bytes,
a Symbol 36 and a Capture 16 on this target. The symbol arena retains 2,359,296
capacity bytes (1,440,000 live payload bytes); capture vectors retain 640,000
capacity bytes (160,000 live payload bytes). Function/scope/name/fact/edge/type
storage is additional. Actual semantic allocator retention is 65,723,629 bytes;
arena accounting estimates 65,723,597 bytes (the difference is small ownership
metadata/string overhead, not an unreported large allocation).

Whole-process peak RSS in a separate sample is 77,100 KiB, including source,
CST, semantic result and temporary maps. RSS timing is a separate noisy sample,
not the median timing above. Stats report allocated/requested bytes rather than
allocator-internal overhead. Imports/external descriptors require additional
storage beyond this no-host workload.

Allocation optimization (same capture workload, seven samples):

| 10,000-function phase | Analysis median ms | Allocation calls | Retained bytes |
| --- | ---: | ---: | ---: |
| Before short-vector optimization (`9dd5170` + probe) | 113.50 | 2,370,115 | 65,723,619 |
| Immediately after optimization | 104.47 | 1,480,115 | 65,723,619 |
| Pre-review `726dbf8` with module identity/budgets | 104.75 | 1,480,118 | 65,723,629 |

Pinned [smallvec 1.15.1](https://docs.rs/smallvec/1.15.1/smallvec/) replaces short
scratch children/token/pattern/edge vectors, leaving output arenas unchanged.
This reduces measured allocation calls by 37.6%; final timing is about 7.7% lower
than the initial sample set. Wall-clock differences also include scheduling
noise. Name interning checks borrowed strings before allocating. Immutable host
signatures share Arc descriptors. Class-member and named-parameter lookup use
maps. Export-name insertion uses sets; reexports and effect summaries use
worklists instead of repeated whole-AST walks.

| Reverse-named signature size | Parse ms | Analysis ms |
| --- | ---: | ---: |
| 1,000 parameters/arguments | 0.79 | 1.37 |
| 5,000 parameters/arguments | 4.41 | 7.27 |
| 10,000 parameters/arguments | 8.05 | 14.67 |

This workload supplies every parameter by name in reverse order. It exercises
real parser argument handling and binder signature lookup, avoiding a hidden
per-argument linear search. It has no captures and does not execute the function.

Reproduce from repository root:

```sh
cargo build -p aside-compiler --example semantic_probe --release --locked
cargo run --locked -p reel-dev -- compiler-probe semantic captures
cargo run --locked -p reel-dev -- compiler-probe semantic named
cargo run --locked -p reel-dev -- compiler-probe semantic captures
```

Bounded behavior uses source tests, not a fabricated HIR. `semantic_depth.rs`
runs in a subprocess: a 50,000-else chain preserves bounded parser diagnostics
and fails the analysis gate; 64 nested valid blocks analyze/drop safely;
50,000 unresolved reads retain exactly four configured source diagnostics.
Node/symbol/capture/work/import budgets are configurable and shared across a
source project, with a total source-admission budget. Resource failure cannot
produce an AnalyzedModule.

Verification on the implementation:

```sh
cargo fmt --all --check
cargo clippy -p aside-compiler --all-targets --locked -- -D warnings
cargo test -p aside-compiler --all-targets --locked
cargo test -p aside-compiler --release --all-targets --locked
cargo run --locked -p reel-dev -- check
```

Debug/release and clippy passed. The 24 bilingual scenes are independently
parsed/analyzed in both editions with explicit native and enclosing lexical
symbols. The 10 immutable accepted T01 conversions retain 32 original assertion
mappings/calls and report parse=passed, analysis=passed, execution=waiting_runtime.
No original golden, author guide, opcode or executor was changed.

## Source-review corrections and shared-signature calls (`5e27085`)

The source-review head retains all original source checks and adds real
counterexamples for nested If/While condition roles, OR/refutable shapes,
per-leaf assignment contracts/duplicates, unknown spreads, export projection,
Map/closure categories, grouped-symbol reexport dependencies, forward signatures,
returning try/loop paths and lexical finally restrictions. Local and export
pattern projection share one grammar-role walk; built-in categories share one
mapping. No parser change was needed for these corrections.

The new `shared` probe declares N defaulted parameters and then calls the same
function N times with no arguments. Source is generated before timing. The
baseline is `726dbf8` plus this exact generator/instrumentation, with no compiler
changes. Each row is the median of seven separate release processes on the same
environment; raw `review-shared-before.csv` and `review-shared.csv` are retained.

| N parameters and N calls | Source bytes | Baseline analysis ms | Cached analysis ms | Cached work units |
| --- | ---: | ---: | ---: | ---: |
| 1000 | 10,899 | 23.39 | 2.23 | 7,001 |
| 5000 | 58,899 | 480.24 | 10.54 | 35,001 |
| 10000 | 118,899 | 1903.68 | 22.51 | 70,001 |

At N=10,000, analysis drops from 1903.68 ms to
22.51 ms (about 84.6 times faster in this sample set).
The old implementation scanned/copied/indexed all parameters for every call,
while reporting only 50,001 work units. The corrected result builds one
signature with 10,000 projected parameters and charges 70,001 units, including
its parameter/index scans. A 100,000-work budget now admits actual bounded work,
rather than hiding approximately 100 million parameter visits. The source test
also verifies that a tighter budget fails before allocating the cached signature.
Host signatures use the same cache and have a separate repeated-call regression.

Cached metadata includes parameter types/names, rest and required indices.
Empty calls with all defaults do not scan parameters or required entries; empty
signatures share one descriptor. Remaining required-argument scans are admitted
before the scan. Function/bound-method keys and shared external descriptor keys
prevent rebuilding maps for ordinary repeated calls. Cache storage is temporary
and dropped before retained-result allocation is measured.

Latest complete source-review measurements (seven processes per size):

| Workload | N | Parse ms | Analysis ms | Semantic retained MiB |
| --- | ---: | ---: | ---: | ---: |
| captures | 1,000 | 3.87 | 11.64 | 5.72 |
| captures | 5,000 | 21.50 | 64.31 | 31.34 |
| captures | 10,000 | 35.71 | 126.16 | 62.68 |
| named | 1,000 | 0.85 | 1.50 | 0.80 |
| named | 5,000 | 4.57 | 7.92 | 4.07 |
| named | 10,000 | 9.18 | 17.78 | 8.14 |
| shared | 1,000 | 0.92 | 2.23 | 1.34 |
| shared | 5,000 | 4.22 | 10.54 | 7.35 |
| shared | 10,000 | 8.59 | 22.51 | 14.70 |

The capture sample now charges 220,000 units at N=10,000 after adding shared
pattern projection. Its retained semantic bytes remain 65,723,629; the latest
whole-process RSS sample is 76,236 KiB. This sample set has higher capture timings
than `726dbf8`; it does not establish a speed improvement for the capture workload.
Scheduling variation and the additional correctness work both affect comparison.
The shared-signature comparison above uses the same generated source and
separately built baseline. These remain compiler measurements, not execution.

Reproduce the corrected shared workload:

```sh
cargo build -p aside-compiler --example semantic_probe --release --locked
cargo run --locked -p reel-dev -- compiler-probe semantic shared
```

The full debug/release suite, strict clippy, fmt and immutable-fixture integrity
checks passed again on the correction. Guide, captures/defaults and T01 sources
remain unchanged; execution remains waiting_runtime.

## Declared contracts, predeclaration admission and lowering references (`95abf2a`)

This latest paired rerun compares `962573b` with `95abf2a`, on the same environment
and unchanged `semantic_probe` source/instrumentation. Each timing has seven
separate release processes. The earlier tables remain historical samples;
`contracts-before-*.csv` and `contracts-after-*.csv` are the current comparison.

| Workload | N | Before analysis ms | After analysis ms | After parse ms | After retained bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| Captures | 1,000 | 10.142 | 9.492 | 3.314 | 5,999,477 |
| Captures | 5,000 | 57.299 | 59.171 | 16.829 | 32,860,757 |
| Captures | 10,000 | 113.099 | 114.017 | 31.915 | 65,723,629 |
| Shared signature | 1,000 | 1.902 | 2.025 | 0.743 | 1,407,893 |
| Shared signature | 5,000 | 10.863 | 9.606 | 3.878 | 7,705,909 |
| Shared signature | 10,000 | 21.349 | 20.182 | 7.817 | 15,412,541 |

The capture N=10k median increases by 0.8%; shared timings vary in both directions
across sizes. These differences do not establish a general speed improvement.
Separating declaration contracts from consumers keeps linear scaling and unchanged
retained memory on these workloads. No-wait declaration flags occupy existing
Function padding on this target; provenance storage is additional for modules/
external descriptors, which these no-host workloads do not exercise.

Capture N=10k still produces 40,000 symbols and 10,000 captures, with symbol/capture
capacity bytes 2,359,296 / 640,000. Live payloads remain 1,440,000 / 160,000 bytes;
whole-process RSS in the new separate sample is 77,000 KiB. Allocation calls are
1,480,132 before and 1,460,133 after: default visibility uses scope boundaries
without copying a parameter-name set, and method-context construction borrows names.
All values include actual source parser→HIR→analysis rather than fabricated HIR.

More complete admission raises charged work from 220,000 to 380,006 for captures
N=10k and from 70,001 to 90,010 for shared N=10k. The latter still builds exactly
one 10,000-parameter signature and passes a 100,000-work budget. Work counts include
attempted admission debits; a rejected debit does not imply the work or allocation
ran. The current reverse-named N=10k sample analyzes in 16.114 ms, with 70,016
charged work units.

`declaration_budget_probe` generates 10,002 named declarations, parses once before
the instrumented region, then runs real HIR projection and semantic analysis for
each budget. The baseline uses `962573b` plus only the exact new probe source;
its implementation is unchanged. Each budget row is a single release observation,
not a timing median. The symbol-capacity column describes the partial arena at
admission failure; failed analyses drop it and cannot produce `AnalyzedModule`.

| Work budget | Before allocated symbols | After allocated symbols | After symbol-capacity bytes | After diagnostic count |
| --- | ---: | ---: | ---: | ---: |
| 0 | 10,002 | 0 | 0 | 1 |
| 1 | 10,002 | 0 | 0 | 1 |
| 5 | 10,002 | 0 | 0 | 1 |
| 100 | 10,002 | 0 | 0 | 1 |
| 10,004 | 10,002 | 0 | 0 | 1 |
| 10,010 | 10,002 | 2 | 144 | 1 |
| 20,000 | 10,002 | 4,997 | 294,912 | 1 |

At zero budget, the baseline failed only after all declarations; the current
version declines the seeded name-index debit before allocating semantic facts,
functions or symbols. It reports 146 retained allocator bytes for the bounded
diagnostic. HIR projection still happens before semantic admission, with its
own node/depth/source limits: this is not a claim of zero parser/projection work.
Partial budgets also admit metadata and symbol work separately before allocation.
The default budget completes all 10,002 symbols in 21.356 ms in this single
observation, charging 80,027 units.

The new context/reference API is checked with actual source: module/function/
generator/NoWait entry contexts, ordinary/async/generator functions, restricted
class methods, field initializers, derive callbacks, host aliases, custom canonical
resolver names, namespace members and multiple aliasing reexports. Declared
contexts remain separate from inferred effects. ABI/capabilities/default linkage
are not benchmarked or implemented here; trusted package linkage remains required.

Reproduce the new admission probe:

```sh
cargo build -p aside-compiler --example declaration_budget_probe --release --locked
./target/release/examples/declaration_budget_probe 10000
```

Rust 1.96.0 workspace fmt, strict all-target clippy, debug and release tests pass.
The immutable T01 integrity check remains 10 sources/32 unchanged assertion maps;
24 bilingual scene pairs and bounded child-process diagnostics pass. Execution
remains `waiting_runtime` and the author-guide sources are unchanged.


## Provenance lifecycle regression after `a50a7ce`

The 10,000-module chain starts with `m0: pub let x=1;`; every later module is
`pub use m<previous>::{x};`. It is legal under the default analysis budgets.
Before this fix, analysis completed but releasing the Arc provenance tail on a
2 MiB debug thread stack aborted with stack overflow. Iterative release now drops
both the ordinary result and a separately retained final shared tail safely.
The sharing case validates all 9,999 hops after result destruction and checks
that the origin's Weak reference can no longer upgrade after final release.

A separate single debug observation measured analysis/result-drop at 951/28 ms;
a single release observation measured 190/24 ms. Shared result-drop plus intact-
history validation and final-tail release took 25 ms debug / 20 ms release.
These are regression observations, not medians or an engine performance claim.
Historical before/after observations and exact commands are preserved in the
[provenance lifecycle log](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/aside-compiler/bench-results/semantic/provenance-lifecycle.txt).

The direct namespace counterexample `a: pub let x=1;` and
`b: use a as ns; let z:int=ns.x;` now retains exactly module a/export=None for the
namespace and a/export=x for the member. Repeated normalization no longer creates
nonexistent export a. Named imports, multi-hop namespace aliases, legal cycles and
a self-module namespace reexport are source-level regressions too.

This fix changes only module reference normalization/destruction plus tests and
compiler evidence. The prior binding, return-contract, budget, Context and capture
implementations are unchanged. Rust 1.96.0 fmt, strict all-target clippy and full
workspace debug/release checks pass, including previous source counterexamples,
24 guide pairs and the immutable 10-source/32-assertion T01 conversion integrity.
Execution remains `waiting_runtime`.
