# Repository validation

Run from the repository root with the workspace toolchain and Cargo.lock.
Generated logs, probes and measurements belong under ignored
`target/reel-artifacts/`.

| Command after `cargo run --locked -p reel-dev --` | Purpose |
| --- | --- |
| `check` | Retained Quirrel assertion mappings and real VM execution |
| `deps` | Workspace dependency boundaries |
| `audit-ui` | UI source ownership, license and provenance |
| `audio-regression [--release]` | Workspace audio-library tests |
| `audio-probe` | PCM allocation and background release probes |
| `root-negative` | Root-accounting negative controls |
| `compiler-probe frontend/semantic/lowering [MODE]` | Compiler work and allocation measurements |
| `measure BUILD_JSON contracts/r01_measurements [TEST_ARGS]` | Resolve exact UI measurement executables from Cargo JSON |

For `r01_measurements`, select ignored cases with `--ignored` or
`--include-ignored` in TEST_ARGS. The tool checks that the named tests completed.
Original fixture sources and oracles are documented
[with the compiler inputs](../aside-compiler/tests/fixtures/quirrel/README.md).

## Self-checking projects

Put executable assertions in the project's `src/main.aside` and return when the
scenario completes. Set `REEL_SCENARIO` to the project directory, then run:

```powershell
$env:REEL_SCENARIO = "D:/path/to/project"
cargo test --locked -p reel-cli --test project_scenarios external_project_scenario -- --ignored --nocapture
```

The host executes source and ASB with independent temporary storage, CPU layout
and offline audio. `REEL_SCENARIO_TIMEOUT_SECS` optionally changes the default
90-second wall-time limit; for example, set it to `600` for a longer scenario.
