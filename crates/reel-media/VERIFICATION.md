# Audio verification boundaries (contract 1.1)

Run from the repository root with pinned Rust 1.96.0 and the root Cargo.lock.
Linux default-feature builds require ALSA development files/pkg-config; feature-off
builds do not depend on CPAL/native audio. Windows GNU checks require the installed
`x86_64-pc-windows-gnu` Rust target. All generated output belongs under ignored
`target/reel-artifacts/`; source documentation does not serve as a current run log.

```sh
cargo fmt --all -- --check
cargo test -p reel-media --locked
cargo test -p reel-media --release --locked
cargo test -p reel-media --no-default-features --locked
cargo test -p reel-media --no-default-features --release --locked
cargo clippy -p reel-media --all-targets --locked -- -D warnings
cargo clippy -p reel-media --no-default-features --all-targets --locked -- -D warnings
cargo check -p reel-media --all-targets --target x86_64-pc-windows-gnu --locked
cargo tree -p reel-media --no-default-features --edges normal --locked
cargo run -p reel-dev --locked -- audio-regression
cargo run -p reel-dev --locked -- audio-regression --release
cargo run -p reel-dev --locked -- audio-probe
cargo run -p reel-media --example device_probe --locked
```

The [Rust validation tool](../../crates/reel-dev/README.md) runs the workspace
audio-library regressions and actual PCM probes. `audio-regression` directly tests
`reel-audio`; no copied workspace, separate lock or source comparison is required.
Original licenses and provenance remain in that crate.

Actual reel-audio/Renderer checks cover mixing and declared stereo→8ch conversion,
finite-value handling, saturation, block splitting, malformed buffers, shortages,
suspend/resume, explicit retirement and budget pressure. The lifecycle fixture
uses the production gate/slot implementation with controlled native failures;
original-Box rejection and retained-callback stop are ownership checks, not device
measurements. `audio-probe` reports real allocator calls and physical worker frees;
its offline stop measurements do not establish native pause/fence timing.

## Native clocks and device scope

CPAL always declares **Unavailable** precision. Generated frames/latency/timestamps
are diagnostics; submitted/presented numeric zeros are invalid sentinels and both
queries return None. They cannot anchor MediaPTS. Native Xrun/EPIPE closes the gate,
invalidates epoch/mappings and software counts, and requires stop/reconfigure.
The pinned ALSA path can silently drop a generated block on zero-progress
Err(EAGAIN); it therefore cannot supply a usable presented count. Actual Ok(0)
is a distinct retry branch. Controlled fault fixtures verify the adapter boundary.

An ALSA virtual null sink can run the real CPAL worker but establishes no audible
output, hardware clock, Windows speaker order or hardware fault recovery. A
missing `/dev/snd` and default-device exit 2 establish device unavailability only.
The controlled OfflineOutput explicitly guarantees submission and permits
Estimated timing; it is separate from the native CPAL capability.

Declared Surround71 order is FL, FR, Center, LFE, BackLeft, BackRight, SideLeft,
SideRight. It verifies the converter, not an actual Windows device mask. The
adapter negotiates known mono/stereo or rejects unknown multichannel-only layouts.
Windows GNU cargo check is compile coverage only; Linux/macOS devices, Windows/N100
8ch hardware, hotplug, native startup/suspend/resume and hardware timing remain
independent target work. No MediaPTS service, decoder, video, DSL executor or full
player is claimed by these focused checks.

The [fixed initial verification record](https://github.com/jhq223/reel/blob/71d9d56e009a3891ad1f6e439f3a7b15db1660ea/crates/reel-media/VERIFICATION.md),
[direct-control retirement record](https://github.com/jhq223/reel/blob/97ecb904ca0ab8cb8aebb1550e87b765e4f894dc/crates/reel-media/RETIREMENT.md)
and [background record](https://github.com/jhq223/reel/blob/1f05685a0267a4bee7ed4394d696c2f68c161cf6/crates/reel-media/BACKGROUND-RECLAIM.md)
retain historical outputs. Their timing/environment statements are not current
verification claims; use the commands above and report the exact tested head.
