# Frontend verification and fixed-source cost

Historical observations below describe their original revisions. Raw evidence links
point to the pre-cleanup Git revision; new generated output belongs under
`target/reel-artifacts/`. Current maintenance commands are documented in
[`crates/reel-dev`](../reel-dev/README.md).

Measured 2026-10-08 on Linux 6.18.44 x86_64, AMD EPYC 9V74, KVM (5 exposed CPUs),
Rust 1.96.0 `ac68faa20`, release defaults. One synchronous source call per sample;
no semantic execution or bytecode is measured. Source construction and a warmup
of both APIs occur before sample regions. stats_alloc instruments the system
allocator with atomic counters, so timings include instrumentation cost.

Implementation/probe SHA-256: `5883859abc3d226d90c1a573fddb5f1c52608a9094a32795393d91c544c3ccb1`. This hashes relative path, NUL,
file bytes, NUL for Cargo.lock, compiler Cargo.toml, examples/frontend_probe.rs
and sorted src/*.rs. Docs/results are excluded, so reporting commits do not
change the measured code identity.

Mixed source: 1024 repetitions of UNIT in frontend_probe.rs, **306176 bytes,
143360 tokens**, SHA-256 `4d91df9df8fbbdd51ad9dfdad0b812a5b5a07fd8c51915e254aa942a411eaf89`. Includes Chinese
comments, functions/defaults, calls/named arguments, children/loops,
interpolation, computed keys, tuples and object/array spread. Repeated function
names are intentional for syntax-only cost; binder acceptance is not claimed.
The 100000-name case is `f(k0:0,...,k99999:0,);`, **888894 bytes, 400004 tokens**,
SHA-256 `e2e114d6bcecedb41f04565ea55a92eec216c5200ae1ee4cd6bd0f285810cb7d`.

| Case | Samples | Median ms | p95 ms | Max ms | Allocation ops | Reallocation ops | Allocated bytes | Retained result bytes | Peak child RSS KiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Mixed lex | 31 | 1.919 | 3.545 | 4.982 | 6146 | 16 | 3194976 | 3145728 | 9088 |
| Mixed parse | 31 | 10.951 | 19.333 | 19.655 | 147567 | 31786 | 11651380 | 3337376 | 9216 |
| 100000 named arguments | 11 | 127.137 | 161.627 | 161.627 | 700070 | 52 | 50239822 | 13600512 | 40260 |

Allocation values are per call (first sample; all raw samples are committed).
stats_alloc's allocated/deallocated counters include reallocation growth/shrink;
net live bytes are allocated minus deallocated. Retained bytes include the
result and exclude prebuilt source, and are **0 after result drop in every
sample**. RSS is the child's high-water mark including source, warmup, runtime
and allocator overhead; it is not an incremental heap peak or cargo's RSS.
p95 uses nearest rank; maxima include scheduler noise. Maximum full-call time
is the measured indivisible interval at this size. Input/token/depth limits
bound work/resources, not a universal wall-clock deadline. No target-device or
whole-VM conclusion is inferred.

## Actual commands and reproduction

This session installed official rustup in /workspace/toolchain. On another
host use the repository's Rust 1.96.0 toolchain. Run from workspace root:

```sh
export PATH="/workspace/toolchain/cargo/bin:$PATH"
export CARGO_HOME=/workspace/toolchain/cargo
export RUSTUP_HOME=/workspace/toolchain/rustup
cargo fmt --all --check
cargo clippy -p aside-compiler --all-targets --locked -- -D warnings
cargo test -p aside-compiler --locked
cargo test -p aside-compiler --release --locked
cargo build -p aside-compiler --example frontend_probe --release --locked
cargo run --locked -p reel-dev -- compiler-probe frontend parse
cargo run --locked -p reel-dev -- compiler-probe frontend lex
cargo run --locked -p reel-dev -- compiler-probe frontend named
cargo build -p aside-compiler --example budget_probe --locked
cargo test --locked -p aside-compiler --test deep_process
```

The Python wrapper uses standard resource for child RSS; GNU time was unavailable.
Historical CSV, RSS and budget outputs are archived in [bench-results](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/aside-compiler/bench-results).

## Behavioral evidence

- All 24 scenes are parsed in both live guides with exact bilingual equality and
  explicit module/story-body contexts. Every UTF-8 boundary prefix of all 24
  scenes is parsed and checked for lossless recovery and diagnostic ranges.
- Tree assertions distinguish zero/one/multiple tuples vs groups, objects vs
  blocks, closure vs logical OR, grouped unions/function types, pattern is/OR,
  match block/object arms, yield/range/assignment and class/head children.
  Destructuring assignment reuses the pattern production, including object,
  default/rest forms; mutable-name eligibility remains a binder rule.
- Numeric/base/int64 minimum/float boundaries and malformed Unicode escapes
  (+, empty digits, surrogates, >10FFFF) test exact ranges. Valid scalar
  boundaries, raw/triple strings and nested comments/interpolation are covered.
- Source maps cover Chinese, emoji, CRLF and byte/scalar/UTF-16 columns; real
  codespan rendering verifies Chinese filename/column output. XID recognition
  uses pinned unicode-ident 1.0.26 / Unicode 18.0.0 even though Logos's regex
  dependency has Unicode 16 tables. New and mixed ASCII/non-ASCII identifiers
  are covered without implicit normalization.
- Seeded arbitrary input at every entry, truncated delimiters/statements,
  retained following declarations, deep arrays/prefixes/flat chains and exact
  token/diagnostic/source limits are checked.
- 100000 unique names plus a duplicate parse within default limits and identify
  the first duplicate range. HashMap lookup replaces the initial linear scan;
  the fixed large-name measurement above retains the language's parameter scope.

## Before/after budget reproduction

The first draft `4f673fe` was run in a detached worktree and isolated children
under a 2 MiB stack with core dumps off. The current budget probe also compiles
against that commit:

```sh
git worktree add --detach /tmp/reel-budget-before 4f673fe
cp crates/aside-compiler/examples/budget_probe.rs /tmp/reel-budget-before/crates/aside-compiler/examples/
CARGO_TARGET_DIR=/tmp/reel-budget-target cargo build --manifest-path /tmp/reel-budget-before/Cargo.toml -p aside-compiler --example budget_probe --locked
cargo test --locked -p aside-compiler --test deep_process
```

[budget-before.txt](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/aside-compiler/bench-results/budget-before.txt) records actual observations:
max_tokens=2 on `"a"` emitted 3 ordinary tokens with exhausted=false;
`\u{+41}` was accepted; 5000 else branches produced depth 5005 and a budget
error only at EOF; 50000 branches aborted from stack overflow (signal 6).
The 5000 case did not crash on this host, so no such claim is made.

[budget-after.txt](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/aside-compiler/bench-results/budget-after.txt): ordinary tokens now obey the
exact emission limit, with at most one separate lossless error-tail leaf. The
same escape diagnoses ASL006. Both 5000/50000 children exit normally with tree
depth 130 and ASR001 at bytes 2019..2023, before the long tail. Unwind/error
nodes may enclose the tree after exhaustion; the complete source is preserved.
Deep-chain regression runs in a child, protecting the test runner from a future
stack-overflow regression.

## Before/after grouped assignment reproduction

The previous baseline `fc418d9` and fix `d453c6a` were built as separate check
CLIs. [assignment-lookahead-before-after.json](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/aside-compiler/bench-results/assignment-lookahead-before-after.json)
retains all 36 actual runs, including source, argv, exit code, complete tree and
diagnostics in both module and body entries. Four grouped location targets
failed before: `(f(a, b).x)`, `(a[(i, j)])`, `([1][0])`, `({x: 0}.x)`.
They now exit 0 with AssignExpr → GroupExpr → MemberExpr/IndexExpr and no
pattern nodes. Controls without nested commas and grouped tuple/array/object
destructuring exit 0 in both revisions; the latter retain GroupPattern trees.
This is source parsing evidence, not executed target evaluation or mutability
validation. That fix was later superseded by the shared grammar trial below after Type/OR
wrappers exposed a regression; these historical observations remain intact.

```sh
git worktree add --detach /tmp/reel-assignment-before fc418d9
CARGO_TARGET_DIR=/tmp/reel-issue8-repro-target cargo build --manifest-path /tmp/reel-assignment-before/Cargo.toml -p aside-compiler --example check --locked
cargo build -p aside-compiler --example check --locked
cargo test --locked -p aside-compiler --test frontend retired_cli_probes_keep_both_entry_points_and_exact_cases
```

## Shared-pattern trial regression and bounds

Fix `46392a80bd96104ed4954500f9a055397e35a0d7` replaces the delimiter/structure
heuristic with a silent trial of the actual pattern/type/default-expression
grammar. [pattern-trial-before-after.json](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/aside-compiler/bench-results/pattern-trial-before-after.json)
records 88 real CLI runs and executable hashes: debug/release, module/body,
before `b6463fa` and after the fix. Typed/OR grouped tuples and arrays failed
before with ASP009, but all four now exit 0 with TypePattern/OrPattern trees.
The four prior location examples and grouped tuple/array/object rest controls
exit 0 in both revisions and keep their respective tree families.

The matrix test compares exact green trees against the pattern entry for
tuple, array/rest, object/default/rest and nested structures, each combined
with plain/type/union/OR/type-OR wrappers, four grouping depths and both entries
(160 assignment scenes). Member/index suffixes, type/OR receivers, failure
diagnostics and continued declarations are checked separately. A default
closure containing another structural assignment exercises nested trials.
Failed trials cannot authorize tuple/array setters as destructuring targets.
Name mutability and equal OR bindings remain subsequent binder checks.

Trials borrow source/tokens and suppress tree emission and diagnostics. All
trials, including nested defaults, share at most `16 * admitted_tokens` token
advances. Completed grammar decisions are cached by significant token offset
and pipe context; storage is bounded by twice the admitted token count.
Recursion inherits the caller's configured depth budget (128 by default).
Resource exhaustion returns ASR001 at the real candidate without consuming
caller input. A forced one-token unit test verifies this boundary, unchanged
cursor/tree/flags, lossless tail and no failed-diagnostic leakage. Cache hits
spend no further trial advances. Wide 10000-name tuples inside 48 groups parse
as both values and assignment patterns; 500 nested closure-default candidates
exit with a lossless budget error.

The fixed-source table above was rerun after this code change. Relative to the
previous recorded mixed parse, allocation operations increased from 127075 to
147567 and allocated bytes from 10305752 to 11651380; retained result bytes
remain 3337376. Timings/RSS are separate instrumented host observations, not
a controlled speed comparison or a target-device guarantee.

```sh
# BEFORE_DEBUG/BEFORE_RELEASE are the separately retained b6463fa check CLIs.
# They can also be rebuilt in a detached b6463fa worktree with a separate target.
cargo build -p aside-compiler --example check --locked
cargo build -p aside-compiler --example check --release --locked
cargo test --locked -p aside-compiler --test frontend retired_cli_probes_keep_both_entry_points_and_exact_cases
```

All owner-resolved forms are implemented and ASP015 removed. Binding/name/type/
effect checks, mutability, lowering, bytecode and execution remain later passes,
not successful features claimed by this source frontend.
