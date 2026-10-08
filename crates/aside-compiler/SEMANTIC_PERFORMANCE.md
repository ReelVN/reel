# L03 semantic analysis: measured source behavior and cost

Rust 1.96.0, Linux x86_64 KVM, AMD EPYC 9V74 (5 visible vCPUs), release.
This measures the compiler, not bytecode or script execution. It does not establish
full-engine, low-end-device, 32-bit, or VM memory results.

`semantic_probe` generates source before timing, runs the real parser, then
projects/analyzes the successful CST. Each row below is the median of seven
separate processes. Raw CSVs and environment metadata are in
[bench-results/semantic](bench-results/semantic/).

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
python3 crates/aside-compiler/bench-results/semantic/run_probe.py \
  target/release/examples/semantic_probe /tmp/l03-captures.csv captures
python3 crates/aside-compiler/bench-results/semantic/run_probe.py \
  target/release/examples/semantic_probe /tmp/l03-named.csv named
python3 crates/aside-compiler/bench-results/semantic/rss_probe.py \
  target/release/examples/semantic_probe > /tmp/l03-rss.csv 2> /tmp/l03-rss.json
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
python3 crates/aside-compiler/tests/fixtures/quirrel/check_integrity.py
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
python3 crates/aside-compiler/bench-results/semantic/run_probe.py \
  target/release/examples/semantic_probe /tmp/l03-shared.csv shared
```

The full debug/release suite, strict clippy, fmt and immutable-fixture integrity
checks passed again on the correction. Guide, captures/defaults and T01 sources
remain unchanged; execution remains waiting_runtime.
