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
| `perf prepare/run/collect/report/runtime --help` | Build, export and measure ordinary games through the distribution player |
| `frame-cost vita/emulator/host INPUT OUTPUT [LIMITS_JSON]` | Per-segment submission intervals and nested phase percentiles from a profiling log |
| `render-cost vita/emulator/host INPUT OUTPUT` | Fixed-frame repetitions, pass/target events, completion observations and trace integrity |

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

Performance collection belongs to the distribution player's `performance`
feature. Each run uses a fresh directory containing the request, exported game,
logs and reports. `prepare` builds the current exporter and exports its shaders
and resources; host runs also build the ordinary `reel-game` executable.

```sh
cargo run --locked -p reel-dev -- perf prepare path/to/game target/reel-artifacts/host-run
cargo run --locked -p reel-dev -- perf run target/reel-artifacts/host-run
```

The workload exits through `game.exit()`. Interactive workloads can be closed
normally. `--vm-timing` enables sampled VM timing; integer VM work counts are
included by default. Reports retain observer cost, incomplete intervals and
capability availability. A player built without `performance`, or started
without a request, follows its ordinary playback path.

Build the Vita development runtime in a configured Linux/VitaSDK shell:

```sh
cargo run --locked -p reel-dev -- perf runtime --output target/reel-artifacts/vita-runtime
```

Install the resulting `reel-vita.vpk` once using the usual Vita or Vita3K
installation workflow. Subsequent runs update that application's runtime files.
Additional cargo-vita options follow `--` on the `runtime` command.

```sh
cargo run --locked -p reel-dev -- perf prepare path/to/game target/reel-artifacts/vita-run --target vita
cargo run --locked -p reel-dev -- perf run target/reel-artifacts/vita-run --runtime target/reel-artifacts/vita-runtime --address DEVICE_IP
```

Use `--vdb PATH` to select vdb. `perf collect RUN --address DEVICE_IP` retrieves
reports after a transport interruption. `perf report RUN` repeats local analysis.
Vita consumes `ux0:/data/reel/performance.json` once and retains it as
`performance.active.json`; reports go to the request's output directory.
Without a request the same player opens its launcher.

For Vita3K, prepare with `--target vita --environment emulator`, then run with
`--emulator PATH_TO_VITA3K --storage CONFIGURED_STORAGE_ROOT --runtime RUNTIME`.
The storage root contains `ux0` and must match Vita3K's configured storage.
Start with Vita3K closed. A per-run configuration preserves the selected base
configuration and binds this storage path; `--emulator-config` selects a base
other than `config.yml` beside Vita3K. Emulator results carry their own label.
`perf collect RUN --storage ROOT` retrieves an emulator run after interruption.

Reports include `summary.json` and a short console view: the slowest sampled
sections, long intervals, main-thread phases, fixed-frame costs and sampling gaps.
Submission rates use accumulated complete intervals, including stalls. Fixed-frame
setup has its own scope; serial pass wall time retains its diagnostic label.

Add `--frame "Exact game title" --accepted-frame 3 --repetitions 3` during
preparation to retain an accepted frame and repeat its rendering. `--cache replay`
clears effect results while preserving resource and target pools; `--cache reuse`
retains effect results. `--serial` adds per-pass completion drains for attribution.
`--save-frame` writes dimensions and a self-contained retained drawing to
`frame.postcard` after measurement, for offline analysis with the same engine build.
This mode currently requires the Vita/GXM adapter. It closes playback after the
repetitions and writes `frozen.tsv`; ordinary session samples go to `frames.tsv`.
`status.json` records terminal state, request and backend capabilities.

Fixed-frame comparisons require matching command/resource identities and render
settings. Serial mode changes batching. Scissor bounds estimate potential work;
SDK-call wall time includes internal waits, and completion observations include
queue/poll delay. GPU hardware timestamps and actual fragment/fetch counts remain
unavailable. The report checks pairing, completion and dropped trace events.

Behavior checks live in the owning crate's Rust tests. The CLI's
[project scenarios](../reel-cli/tests/project_scenarios.rs) exercise source/ASB,
save restoration, accepted input and offline audio through the shared player.

Run crate checks with `cargo test --locked -p <crate>`; `--all-targets` includes
Rust examples in compile and lint commands. Compiler execution fixtures are
documented [beside their inputs](../aside-compiler/tests/fixtures/quirrel/README.md).
Video tests require `cargo test --locked -p reel-media --features video`.

Vita packages read fixed assets from `crates/reel-player/runtime/vita/` and
`crates/reel-dev/runtime/vita/device-checks/`. LiveArea images and the launcher font archive
are maintained there. To update the launcher's font and native modules, run:

```sh
cargo run --locked -p reel-dev -- vita-assets --modules path/to/module \
    --font path/to/font.ttf --font-license path/to/license.txt
```

Low-level device contracts use `reel-device-checks` with feature `device-checks`.
Its case file is `ux0:/data/reel-device-checks/case.txt`, for example
`--case effect_region`. Pixel assertions and completion/resource retirement
checks run independently of game performance collection. The media contract is
an ordinary project at `fixtures/media_contract`; its source lists the inputs
and export command. Cargo's configured target directory owns VPK outputs.

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
