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

Build compiler probes first, for example:

```sh
cargo build --locked -p aside-compiler --release --example lowering_probe
cargo run --locked -p reel-dev -- compiler-probe lowering
```

The compiler probe command currently resolves Unix executable paths. For UI
measurements, select ignored cases with `--ignored` or `--include-ignored`.
Logs and measurements are written under `target/reel-artifacts/`.

Behavior checks live in the owning crate's Rust tests. The CLI's
[project scenarios](../reel-cli/tests/project_scenarios.rs) exercise source/ASB,
save restoration, accepted input and offline audio through the shared player.

Run crate checks with `cargo test --locked -p <crate>`; `--all-targets` includes
Rust examples in compile and lint commands. Compiler execution fixtures are
documented [beside their inputs](../aside-compiler/tests/fixtures/quirrel/README.md).
Video tests require `cargo test --locked -p reel-media --features video`.

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
