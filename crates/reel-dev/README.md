# Repository tools

Run these commands from the repository root with the workspace toolchain and lockfile.

| Command after `cargo run --locked -p reel-dev --` | Purpose |
| --- | --- |
| `check` | Quirrel-derived assertion and VM execution checks |
| `deps` | Shared UI types and platform dependency boundaries |
| `audio-regression [--release]` | Audio-library regression checks |
| `audio-probe` | PCM allocation and background release measurements |
| `root-negative` | Mutation checks for GC root accounting; requires Bash |
| `compiler-probe frontend/semantic/lowering [MODE]` | Compiler work and allocation measurements; requires GNU time |
| `measure BUILD_JSON contracts/r01_measurements [TEST_ARGS]` | UI measurements from Cargo JSON; requires GNU time |
| `frame-cost vita/emulator/host INPUT OUTPUT [LIMITS_JSON]` | Per-segment submission intervals and nested phase percentiles from a profiling log |

Build compiler probes first, for example:

```sh
cargo build --locked -p aside-compiler --release --example lowering_probe
cargo run --locked -p reel-dev -- compiler-probe lowering
```

The compiler probe command currently resolves Unix executable paths. For UI
measurements, select ignored cases with `--ignored` or `--include-ignored`.
Logs and measurements are written under `target/reel-artifacts/`.

`frame-cost` reads the latest complete version-1 frame report; an incomplete newer
report is an error. Limits are an optional JSON map such as
`{"session_us":6000,"vm_slice_us":3000}` in microseconds. First partial intervals
and loops containing multiple submissions are identified separately. The report
records declared environment, retention losses and sample size; GPU service time
and display cadence require their own measurements. Nested phase percentiles are
reported separately.

Build the probe with `reel-player/vm-execution-counts` to append integer VM work
deltas at submission boundaries. The report validates opcode totals and keeps
these quantities separate from phase durations; frame-slot counts also include
host-side access. Label boundaries and restored VM identities start fresh deltas.

Add `reel-player/vm-profiling` for sampled whole-slice ledgers at those same
boundaries. `frame-cost` checks their nested sums and reports raw sampled totals,
clock-query counts and empty-clock calibration. Observer cost is included;
sampled proportions describe the instrumented slices. Independent legacy step
samples overlap and cannot be added to the ledger.

Behavior checks live in the owning crate's Rust tests. The CLI's
[project scenarios](../reel-cli/tests/project_scenarios.rs) exercise source/ASB,
save restoration, accepted input and offline audio through the shared player.

Run crate checks with `cargo test --locked -p <crate>`; `--all-targets` includes
Rust examples in compile and lint commands. Compiler execution fixtures are
documented [beside their inputs](../aside-compiler/tests/fixtures/quirrel/README.md).
Video tests require `cargo test --locked -p reel-media --features video`.

Vita packages read fixed assets from `crates/reel-player/runtime/vita/` and
`crates/reel-dev/runtime/vita/`. LiveArea images and the launcher font archive
are maintained there. To update the launcher's font and native modules, run:

```sh
cargo run --locked -p reel-dev -- vita-assets --modules path/to/module \
    --font path/to/font.ttf --font-license path/to/license.txt
```

The probe's ignored `authored/` and `media/` directories receive exported
project packages; its `module/` receives the matching native modules.
Build each application with `cargo +nightly vita build vpk --release --locked`
and its package, binary and feature selection. Cargo's configured target
directory owns the resulting VPK and executable files.

Native diagnostics use the target display and audio device:

```sh
cargo run --locked -p reel-player --features window --bin reel-diagnostic
cargo run --locked -p reel-player --features window --bin reel-session-host
cargo run --locked -p reel-media --example device_probe
```

Set `REEL_DIAGNOSTIC_FONT` to a TTF/OTF path. The session host first reads
`REEL_SESSION_FONT`, then falls back to that path. F5 recreates the graphics
device; F6 suspends the surface. `REEL_DIAGNOSTIC_TRACE=1` logs frame, epoch and
pointer routing.
