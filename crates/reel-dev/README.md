# Repository validation

Run from the repository root with the workspace Rust toolchain. All generated
logs, probes and measurements belong under ignored `target/reel-artifacts/`.

```sh
cargo run --locked -p reel-dev -- check
cargo run --locked -p reel-dev -- deps
cargo run --locked -p reel-dev -- audit-ui
cargo run --locked -p reel-dev -- audio-regression
cargo run --locked -p reel-dev -- audio-regression --release
cargo run --locked -p reel-dev -- audio-probe
cargo run --locked -p reel-dev -- root-negative
cargo run --locked -p reel-dev -- compiler-probe frontend
cargo run --locked -p reel-dev -- compiler-probe semantic
cargo run --locked -p reel-dev -- compiler-probe lowering
```

`check` verifies retained Quirrel assertion mappings and executes the registered
Aside migrations through B01 and the real VM. `deps` verifies module boundaries;
`audit-ui` verifies one owned UI implementation and its licenses/provenance.
`audio-regression` runs the workspace reel-audio tests directly. `audio-probe`
checks real PCM mixing and background release; it does not verify an audio device.
`root-negative` verifies root-accounting controls. Compiler probes record work
and allocation costs; their optional MODE is passed to the selected probe.

For UI/render measurements, capture Cargo JSON for the desired profile, then run
`cargo run --locked -p reel-dev -- measure BUILD_JSON contracts/r01_measurements [TEST_ARGS]`.
The tool resolves the exact artifact from Cargo output, not a guessed executable.

Borrowed runtime code is maintained in workspace crates. There is no fork-copy,
source-hash gate or C++ compatibility runner. Original Quirrel inputs and output
oracles remain in [compiler fixtures](../aside-compiler/tests/fixtures/quirrel/README.md).
