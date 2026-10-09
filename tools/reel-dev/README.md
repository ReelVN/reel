# Reel development checks

Run from the repository root with pinned Rust 1.96.0. Production crates and the
four maintained Masonry crates use one workspace and root lockfile. The historical
U01 upstream/experimental comparison remains explicitly excluded and isolated.
No command below dispatches Aside bytecode. Parsing, binding and codec checks are
not execution coverage. Native UI checks require Fontconfig/pkg-config on Linux;
process RSS measurements additionally require GNU `time` on PATH.

```sh
cargo run --locked -p reel-dev -- check
cargo test --locked -p reel-dev
cargo run --locked -p reel-dev -- deps
cargo run --locked -p reel-dev -- inventory
cargo run --locked -p reel-dev -- audit-masonry /path/to/xilem-271a27a6
cargo test --locked -p tree_arena --test lookup_depth -- --nocapture
cargo test --locked -p tree_arena --test lookup_depth --release -- --nocapture
cargo test --locked -p tree_arena --no-default-features --features safe_tree --lib
cargo run --locked -p reel-dev -- root-negative
```

`check` reads pinned source, recipes, derivatives and T01 maps without writing.
`convert --write` explicitly regenerates reviewed derivatives; it rejects changed
source hashes, token-interior edits and changed/reordered/removed assertion tokens.
`inventory` writes a source inventory with six classifications, 1,296 fixtures
and explicit `not_run`/`waiting_runtime` states. It never infers execution success.

All logs, JSON/CSV observations, generated source copies and source diffs belong
under `target/reel-artifacts/`. They are ignored. Curated test inputs, original
source hashes, licenses, recipes, source maps and bytecode fixtures remain tracked.
Historical observations are available in Git history; a compact immutable raw
observation fixture retains eight real Quirrel mismatches and one passing fixture's
three seeds. Full observations can be checked without executing any source:

```sh
cargo run --locked -p reel-dev -- verify-results target/reel-artifacts/quirrel-results --t01-baseline
```

That command checks every raw ZIP hash and original line comparison, readable
failure bytes, return codes, runner exit, snapshot/golden changes, exact fixture
universe and seed sets, and the recipe/frontend hash correspondence. Ordinary
Rust tests exercise the compact fixture and reject forged hashes/statuses/exits.
`--t01-baseline` also requires exactly eight mismatches and runner exit 1.

When a new upstream observation is explicitly needed, the first-party observer is
now Rust. Build the upstream `sq` separately and invoke:

```sh
cargo run --locked -p reel-dev -- run-upstream \
  --sq target/reel-artifacts/quirrel-build/bin/sq \
  --out target/reel-artifacts/quirrel-results-new \
  --work target/reel-artifacts/quirrel-work-new
```

This command executes Quirrel fixtures. It ports the fixed `testRunner.py`
contract; the imported Python runner remains unchanged as the behavior reference.
The six roots, first-dot file selection, flags, 12 workers, default/0/255 seed
selection and per-fixture order, 10-second child timeout, line count and Python
`rstrip` comparison are retained. Exec/type stdout and stderr share a file;
diagnostic/AST/analyzer streams are recorded separately. Every seed's actual bytes
and SHA are captured before reuse, ZIP entries retain their 1980 timestamp, and
non-passing actual output remains readable. Missing goldens are created only in
the disposable copy, recorded as `unasserted`, and fail the bridge's drift check.
Normal runner exits remain 0/1; observer exceptions retain summary code 2 and
command exit 1. The original snapshot is checked before and after observation.

Output and work must be new, non-overlapping directories below the artifact root.
Both version queries are now bounded by 10 seconds and retain raw version output;
an invalid final version or snapshot error fails after saving available evidence.
The log is a Rust record listing rather than a copy of Python's console formatting;
platform/observer metadata identifies Rust instead of a Python interpreter.
On Unix, owned process groups let a timeout terminate wrappers and descendants,
and the deadline includes draining inherited pipes. Windows termination uses
`taskkill /T /F`; Windows execution has not been verified.

Migration checks use mock subprocesses and synthetic fixtures, including crashes,
timeouts, spawn/decode errors, missing goldens, snapshot drift, byte/SHA/ZIP checks,
12 concurrent workers and sequential seeds. A fixed digest also checks all 1,494
planned invocations against the historical observations for 1,296 fixtures.
The Rust observer has **not** replayed the real Quirrel test suite; those mock and
plan checks do not establish actual upstream execution equivalence.

Compiler probes retain their original Rust allocation assertions and workloads:

```sh
cargo build --locked -p aside-compiler --release --examples
cargo run --locked -p reel-dev -- compiler-probe frontend parse
cargo run --locked -p reel-dev -- compiler-probe semantic captures
cargo run --locked -p reel-dev -- compiler-probe lowering
```

The tool measures already-built children, records raw stdout/stderr (including
GNU time's RSS and user/system time), repeats semantic/lowering cases seven times,
and saves medians, rejection records, revision and toolchain. Compiler capacity
upper bounds remain distinct from whole-child RSS. Frontend probes provide their
own internal repetitions. The compiler and any VM execution are outside RSS scope.
Semantic children retain the original 30-second bound; `deep_process.rs` retains
the budget probe's 15-second bound. Both have short-deadline mock regression tests.
Semantic failures save exact stdout/stderr byte files, exit/signal and timeout
records before failing, and a timeout terminates the whole owned process group.

Stage measurements resolve the exact package/target executable from Cargo JSON:

```sh
mkdir -p target/reel-artifacts
cargo test --locked -p reel-stage --test contracts --release --no-run --message-format=json > target/reel-artifacts/stage-build.jsonl
cargo run --locked -p reel-dev -- measure target/reel-artifacts/stage-build.jsonl contracts
cargo test --locked -p reel-stage --test r01_measurements --release --no-run --message-format=json > target/reel-artifacts/r01-build.jsonl
REEL_R01_PROBE_CASE=rect cargo run --locked -p reel-dev -- measure target/reel-artifacts/r01-build.jsonl r01_measurements --ignored
REEL_R01_PROBE_CASE=path cargo run --locked -p reel-dev -- measure target/reel-artifacts/r01-build.jsonl r01_measurements --ignored
cargo run --locked -p reel-dev -- measure target/reel-artifacts/r01-build.jsonl r01_measurements --launch-control
```

The launch control runs `/bin/true` through the same GNU time launcher. Child exit
failures propagate. RSS excludes the Cargo/compiler process. Deep-tree tests build
on a separate large stack and exclude destruction; both profiles together retain
the six small-stack find/find_mut cases, with capacities 1/32/128/depth and zero
allocation/reallocation checks. The historical old/recursive source comparisons
remain in Git history; current regression checks do not need obsolete commits.

## First-party Python disposition

Paths below identify the former scripts. No assertion is removed merely to reduce
script count. First-party Python has been migrated or retired; the final upstream
observation bridge is replaced by `run-upstream` with the contract and verification
limits above. The two upstream Python files are untouched and remain covered by
`UPSTREAM.json`.

| Former script | Replacement / retained coverage |
| --- | --- |
| compiler `bench-results/run_probe.py` | `compiler-probe frontend`; existing `frontend_probe.rs` assertions, child RSS/user/system time |
| compiler `bench-results/semantic/run_probe.py` | `compiler-probe semantic`; 1k/5k/10k, seven samples, raw bytes/exit/timeout, original 30-second bound |
| compiler `bench-results/semantic/rss_probe.py` | same command; every child has separate GNU time RSS |
| compiler `bench-results/lowering/run_probe.py` | `compiler-probe lowering`; all five size grids, seven samples, medians/rejections/environment |
| compiler `bench-results/run_budget_probe.py` | `deep_process.rs`: explicit 5k/50k, 2 MiB stack checks and original 15-second bound; historical binary comparison retired |
| compiler `bench-results/run_assignment_probe.py` | exact cases and both entries in `frontend.rs`; obsolete before-binary observation retired |
| compiler `bench-results/run_pattern_trial_probe.py` | exact type/OR/location cases and both entries in `frontend.rs`, plus existing full pattern grammar trials |
| compiler fixtures `check_integrity.py` | `fixtures::immutable_t01`, source/map pins, accepted T01, 10 sources/32 assertions/waiting_runtime |
| VM `run_root_range_negative_controls.py` | `root-negative`; all nine precise failure checks in an isolated copy, original files unchanged |
| stage `audit-source.py` | `audit-masonry`; upstream hashes, original licenses/headers, exact Rust closure, generated current diff |
| stage `check-tree-depth.py` | `tree_arena/tests/lookup_depth.rs`; all current stack/capacity/allocation cases |
| stage `measure.py` / `measure-r01.py` | `measure`; exact Cargo executable, rect/path, launch control, child RSS/exit |
| Masonry experiment `prepare.py` | `prepare-masonry UPSTREAM`; hash verification before copying, patch dry-run/application, original locks and isolated baseline/experiment |
| Masonry experiment `check-deps.py` | `deps MANIFEST`; isolated root, unique UI types and native normal/build dependency closure |
| Quirrel `lexical.py` / `convert.py` | Rust byte scanner/converter; source pins, UTF-8, complete bidirectional maps and all assertion/boundary negative cases |
| Quirrel `inventory.py` | `inventory` plus `verify-results`; six categories, exact 1,296 universe, faithful tested seed predicate, frontend/recipe checks |
| Quirrel `test_integrity.py` | Rust fixture tests plus full `verify-results`; original ZIP/SHA/mismatch and all conversion checks |
| Quirrel `check_frontend.py` | existing `semantic_quirrel` compiler integration tests; obsolete external fc418d9 project/lock retired |
| Quirrel `run_upstream.py` | `run-upstream`; fixed upstream plan, 12 workers, seed order, raw byte/SHA/ZIP observations, comparison/status/exit and disposable-copy checks; mock verification only, real replay unverified |
