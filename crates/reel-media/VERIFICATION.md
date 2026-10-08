# Issue #10 initial verification record (historical)

This records the initial implementation at `71d9d56e009a3891ad1f6e439f3a7b15db1660ea`. The approved Kira fork, explicit reclamation API, current measurements and CPAL Unavailable clock policy supersede its dependency and clock statements; see [RETIREMENT.md](RETIREMENT.md) and the current [README](README.md). The historical CPAL null-sink run below does not establish physical device playback.

Validated 2026-10-08 on Debian 13 x86_64, a KVM container exposing 5 vCPUs of an AMD EPYC 9V74. This is not Windows, N100 or an audio-hardware latency run. `/dev/snd` is absent. Production source and its contracts are in the preceding implementation commits; this file records outcomes, not acceptance by the integration owner.

## Environment preparation actually used

The workspace initially had no Rust compiler or ALSA development package. Installed the official toolchain into the writable workspace, with no repository toolchain modifications:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/reel-rustup.sh
RUSTUP_HOME=/workspace/rustup CARGO_HOME=/workspace/cargo \
  sh /tmp/reel-rustup.sh -y --no-modify-path --default-toolchain 1.96.0 \
  --profile minimal --component rustfmt,clippy
```

`rustc -Vv`: `rustc 1.96.0 (ac68faa20 2026-05-25)`, host `x86_64-unknown-linux-gnu`, LLVM 22.1.2. The existing repository `rust-toolchain.toml` remains unchanged.

`apt-get download libasound2-dev` could not locate the package in the environment's apt cache. Downloaded Debian's official `libasound2-dev_1.2.14-1+deb13u1_amd64.deb` from `https://deb.debian.org/debian/pool/main/a/alsa-lib/`, extracted it with `dpkg-deb -x` into `/workspace/alsa`, and copied the already-installed `libasound.so.2.0.0` there for linking. These are environment dependencies, not repository files. Build environment:

```sh
export RUSTUP_HOME=/workspace/rustup
export CARGO_HOME=/workspace/cargo
export PATH=/workspace/cargo/bin:$PATH
export CARGO_TARGET_DIR=/workspace/reel-build
export PKG_CONFIG_PATH=/workspace/alsa/usr/lib/x86_64-linux-gnu/pkgconfig
export PKG_CONFIG_SYSROOT_DIR=/workspace/alsa
rustup target add x86_64-pc-windows-gnu --toolchain 1.96.0
```

At that initial revision, dependencies came from pinned ordinary crates.io packages and inspected upstream sources were not imported. The later approved minimal Kira fork is recorded separately. No relevant SKILL.md existed under the provided workspace `.agents/skills` or repository `.agents` paths. Read AGENTS.md, the AudioOutput/PCM/lifecycle/clock sections of engine-architecture.md, aside-language.md and the relevant Chinese creator-guide media section before implementation.

## Contract and build commands

All these completed successfully after the fixed control-owned FillSlot change:

```sh
cargo test -p reel-media --locked
cargo test -p reel-media --no-default-features --locked
cargo clippy -p reel-media --all-targets --locked -- -D warnings
cargo clippy -p reel-media --no-default-features --all-targets --locked -- -D warnings
cargo fmt -p reel-media -- --check
cargo check -p reel-media --all-targets --target x86_64-pc-windows-gnu --locked
cargo tree -p reel-media --no-default-features --edges normal
crates/reel-media/tests/support/run_pcm_probe.sh
```

The behavior evidence is:

- **Real Kira mix:** two StaticSounds `[0.2,-0.1]` + `[0.3,-0.2]` yield steady output `[0.5,-0.3,0,0,0,0,0,0]` in declared 8ch/48kHz/f32. A device block larger than max_pull_frames=7 exercises splitting. The short Kira interpolation startup is handled explicitly in assertions.
- **Boundary conversion:** f32/f64/i16/u16/i32, mono average, NaN/Infinity, saturation, unsigned silence, unfilled scratch reuse and incomplete 8-channel frame. Incomplete/mismatched device data yields whole-buffer silence without invoking fill, DeviceContract failure and an unavailable new-epoch clock.
- **Ownership:** injected build/play/verify refusals return the identical Box pointer with calls=0, drops=0. The returned fill remains callable by its owner and is destroyed once. Early callbacks during build/play cannot call it. A successful native play followed by an injected asynchronous fault is accepted ownership, with no fill call and retention until stop.
- **Fence and recovery:** deterministic blocked test fill proves suspend/stop do not return until the active call ends; entry remains closed while waiting. Suspend retains the closure, stops content position, and resume reuses it. Suspend/pause or resume/verify failures retain ownership until stop. A test native API deliberately keeps its callback object after stop and fails pause: the fill destructor still runs on the stop caller before return; that retained callback then only writes silence. No native teardown acknowledgement is needed for fill ownership.
- **Epoch:** old stream faults cannot mutate a rebuilt stream's latch; a format change invalidates the clock, requires stop, and allows explicit configure/new Kira at 44.1kHz/i16/mono. A clock is always read with its OutputEpoch and availability. No business completion events or DSL activities are produced by this output-level crate.
- **Shortage and pressure:** a capacity-two rtrb PCM queue rejects a third frame, supplies valid stereo frames followed by zeroes, and counts 3 missing frames in a 5-frame block. A further empty block moves submitted device frames to 10 while consuming no queued content. Full Kira play capacity returns the original budgeted sound; coalesced volume/stop remain usable and deferred budget reclamation reaches zero after teardown.
- **Notification:** a realtime fault wakes the injected control WakeSignal without polling AudioOutput. The generic wake function executes on the dedicated notifier. Realtime scheduling denial is visible as a counter while the accepted stream remains Running.

No-default-features normal dependency tree:

```text
reel-media
├── atomic_refcell 0.1.13
├── kira 0.12.5
│   ├── atomic-arena 0.1.2
│   ├── glam 0.33.12 / mint 0.5.9
│   ├── pastey 0.2.3 (proc-macro)
│   ├── rtrb 0.4.0
│   └── triple_buffer 9.0.0 / crossbeam-utils 0.8.23
├── reel-core (accepted D01, unchanged)
├── rtrb 0.4.0
├── sync_wrapper 1.0.2
└── triple_buffer 9.0.0
```

There is no CPAL/ALSA/Windows platform dependency, winit, wgpu, decoder or VM in this active feature-off tree. Cargo.lock necessarily contains CPAL's target-dependent resolution entries even when a feature is disabled; this does not mean those packages are compiled into the feature-off build.

## Real Kira allocation and cost probe

The standalone probe builds the production crate separately in debug and release, then instruments only the thread executing the actual fill/conversion calls. It uses two looping real Kira StaticSounds at `[0.8,-0.8]` and `[0.7,-0.7]`, plus a finite sound to exercise deferred retirement, for 4096 blocks × 512 frames (2,097,152 frames). Overload is limited by real Kira and converted to i16; no substitute mixer is involved.

Final run samples:

```text
real_kira blocks=4096 frames_per_block=512 device=8ch/48000Hz/i16 mixer=2ch/f32 samples=[32767,-32768,0,0,0,0,0,0]
# debug
callback alloc=0 realloc=0 dealloc=0 p95_us=520.055 p99_us=790.223 max_us=2751.574 block_deadline_us=10666.667 elapsed_ms=1488.791
# release
callback alloc=0 realloc=0 dealloc=0 p95_us=44.527 p99_us=110.266 max_us=1174.533 block_deadline_us=10666.667 elapsed_ms=121.773
clock epoch=4 submitted=2097152 estimated_presented=2096128 latency=Some(512) uncertainty=Some(0) precision=Estimated
lifecycle suspended_position=0.680000000 stopped_position=0.690666667 stop_prevents_fill=true; cpu_pcm_budget_peak_bytes=1537024
```

The 512-frame deadline at 48kHz is 10.667ms. This offline workload stayed within it in both profiles in this run. It establishes neither an OS scheduling guarantee nor N100/device tail latency. Host noise changed percentiles between earlier runs; the final figures are recorded rather than choosing the lowest observed result. The explicit injected latency is one prior block, so estimated_presented is two blocks behind the submitted end at callback entry. It is not a measured hardware playback count.

Memory bounds: default conversion scratch 4096 bytes; three Kira Frame buffers at 128 frames total 3072 bytes; fixed-capacity command/arena metadata in addition. Decoded PCM ticket peak in this workload was 1,537,024 bytes, including conservative construction overlap. Default sound capacity is 64, maximum 256; optional queued PCM is at most 96,000×8=768,000 sample bytes plus fixed queue metadata. Aggregate decoded residency is bounded by the caller's BudgetPool; the implementation does not claim that sample tickets cover all allocator overhead.

## Actual CPAL runs in this environment

The physical default-device command was actually run:

```sh
cargo run -p reel-media --example device_probe --locked
```

It exited **2** with ALSA reporting `cannot find card '0'` / `Unknown PCM default` and:

```text
device_unavailable: Unavailable: The requested audio device is not available. It may have been disconnected.
```

This confirms the unavailable path, not successful device playback.

Separately created a temporary ALSA null configuration (not a repository file):

```sh
cat > /tmp/reel-alsa-null.conf <<'CONFIG'
pcm.!default { type null }
CONFIG
ALSA_CONFIG_PATH=/tmp/reel-alsa-null.conf \
  cargo run -p reel-media --example device_probe --locked
```

This uses the real CPAL ALSA worker and real Kira Renderer, with a **virtual null sink**. The final command exited 0 and observed:

```text
negotiated=AudioFormat { sample_rate_hz: 48000, mixer_channels: 2, device_sample_format: F32, device_channel_layout: Stereo, max_pull_frames: 512, channel_map: [Left, Right] }
status=AudioStatus { state: Running, output_epoch: OutputEpoch(4), underrun_count: 0, silent_frames: 0, realtime_denied_count: 0, latest_device_error: None }
stop=Err(AudioError { kind: DeviceFailure, message: "ALSA function 'snd_pcm_pause' failed with error 'Unknown errno (77)'" })
fill_calls=41662 post_stop_fill_calls=0
```

The example asserts Stopped and Unavailable clock after that real native pause error and checks the fill counter again after 20ms. A null sink can run much faster than physical time; its callback count, xrun count, clock estimates and promotion eligibility are not audio-hardware measurements. CPAL intentionally avoids realtime promotion of ALSA NULL devices. No physical start failure, hardware suspend/resume, hot unplug or route change was injected here; those failure branches are controlled fixtures plus versioned source review.

## Remaining target work and review decisions

- Integration-owner review/rerun of start refusal, mutable borrow release before ACTIVE release, direct control-side fill disposal, Kira deferred budget retirement and clock precision.
- Physical Windows x64/N100 8ch/48kHz/f32 device: actual speaker mask/order, selected format, callback buffer distribution, audible output, xruns and tail latency. Current CPAL API lacks a public ordered layout, and this adapter deliberately rejects multichannel-only unknown layouts. Candidate follow-up: query Windows native WAVEFORMATEXTENSIBLE speaker mask and guarantee the configured direct-out order, or obtain an upstream explicit layout API. Ordinary known-stereo renegotiation is implemented now. No guessed mapping is allowed.
- Actual Linux/macOS devices, native startup failure, suspend/resume, hardware disconnect/default-route changes, clock error bounds. macOS source was reviewed but no SDK/build/runtime was available; Windows was **cross-checked**, not linked, launched or device-tested.
- No MediaPTS synchronization service, streaming decoder, video, story/DSL or complete player is claimed here. The bounded PCM bridge exposes shortage behavior for later producers; logical activities/rebinding belong to their own integration work. Creator guides remain maintained by the integration owner as requested.

No main branch modification, remote merge, Actions run, PR readiness conversion or guide expansion was performed. The delivery is a draft PR against the accepted D01 dependency branch, with its exact baseline recorded.
