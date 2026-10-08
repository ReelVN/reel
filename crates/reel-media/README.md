# reel-media: AudioOutput / Kira bridge (contract 1.1)

Issue #10 implementation, based on accepted D01 `c03226a59a9dd4f8b448d4eea82262a1d6e57ef8` (`issue-1-core-contracts`, Rust 1.96.0). This branch changes only this crate and dependency lock entries. The integration owner maintains both creator guides.

## Review entry points

- `src/contracts.rs`: engine-owned AudioOutput, PcmFill, configuration, ordered channel map, status and clock snapshot types.
- `src/mixer.rs`: private Kira `Backend::setup(u32, usize)` / `start(Renderer)` implementation. The real Renderer calls `on_start_processing()` and `process(out, 2)`. `KiraMixer::take_fill()` is a linear handoff; keep the manager alive for control and deferred resource reclamation.
- `src/output.rs`: common lifecycle and callback used by OfflineOutput and CpalOutput. `DrivenOutput::start`, `Shared::enter/close_and_wait/open`, `CallGuard::drop`, and `DrivenOutput::stop` are the ownership review points.
- `src/desktop.rs`: actual `DeviceTrait::build_output_stream`, `StreamTrait::play/pause` calls and native error classification.
- `src/pcm.rs`: prezero, block splitting, finite sample sanitation, explicit mapping and integer saturation.

Pinned registry dependencies: Kira **0.12.5** (default features disabled), CPAL **0.18.1** (optional `cpal-output`, desktop targets, `realtime` promotion attempts), rtrb **0.4.0**, triple_buffer **9.0.0**. The last two are also Kira's upstream dependencies. No vendored ecosystem. Realtime scheduling denial is counted and does not discard the mixer. No window, renderer, decoder or DSL dependency is introduced.

## Ownership and native API evidence

1. `configure` requires Stopped and negotiates a real format/layout pair. Mixer channels are always 2. Initialize Kira only after this returns the actual device sample rate.
2. `start` keeps the original PcmFill in its stack while verifying the device, building the native stream and calling native `play()`. The callback has only an empty capacity-one rtrb Consumer and a CLOSED entry gate. Even a callback during build/play can only write silence. Any synchronous refusal returns the **same Box**, never invoked.
3. After native `play()` returns Ok, start pushes fill once, records ownership and opens entry. An asynchronous native error can instead latch FAULT; opening uses compare_exchange so it cannot clear that fault. The accepted fill remains owned until stop, even if never first invoked.
4. Only a successful 0→ACTIVE exchange permits a call. suspend/stop atomically add CLOSED and wait for ACTIVE to clear. CallGuard releases ACTIVE when the entire native buffer finishes. This control-side wait is solely the required in-flight fence; the realtime thread never waits for the main thread, acquires a callback Mutex or participates in a command acknowledgement handshake.
5. Successful suspend waits for the active call, pauses the native stream and retains the same closure/Renderer. Kira's logical content position freezes because no Renderer calls occur. Resume verifies device identity, default format and support for the negotiated pair, then plays and reopens the same gate. Failure remains Failed with retained fill, until stop.
6. Stop closes entry and fences the active call before attempting native pause or destroying the stream. A pause failure still destroys the stream and returns Stopped. CPAL's ALSA/WASAPI Drop joins its worker; on CoreAudio the gate independently prevents later fill calls, including a callback between gate closure and AudioUnit destruction. Fill and Renderer are disposed with that stream's callback. There is no guarantee that a native device has physically drained every previously submitted sample on stop.
7. Each new native stream gets its own gate/fault latch. Late old-stream errors cannot close a replacement stream or change its epoch. Generic WakeSignal notification runs on a dedicated control notifier (2ms polling of a coalesced atomic flag), not in the realtime callback. Host code must acknowledge WakeSignal before draining as D01 specifies.

Official versioned sources used for the interface audit:

- [Kira Backend](https://docs.rs/crate/kira/0.12.5/source/src/backend.rs), [Renderer](https://docs.rs/crate/kira/0.12.5/source/src/backend/renderer.rs): owned Renderer and the real stereo process entry.
- [Kira resource storage](https://docs.rs/crate/kira/0.12.5/source/src/backend/resources.rs): finished Sound moves into a bounded unused queue; `ResourceController::insert_with_key/remove_unused` reclaims it on the control side. BudgetedSound delegates to Kira's StaticSound, and its shared BudgetTicket stays charged through that deferred residency. Stop teardown may destroy the renderer on a native worker after PCM processing ends.
- [CPAL traits](https://docs.rs/crate/cpal/0.18.1/source/src/traits.rs): build consumes a FnMut callback, returns a Stream, and play/pause return Result. There is no generic callback recovery API for a failed build and no public ordered speaker layout API.
- [CPAL ALSA](https://docs.rs/crate/cpal/0.18.1/source/src/host/alsa/mod.rs): `StreamTrait::play` releases its worker latch and can fail on ALSA start/unpause; `pause` can be unsupported or fail; `Stream::drop` wakes and joins the worker.
- [CPAL WASAPI stream](https://docs.rs/crate/cpal/0.18.1/source/src/host/wasapi/stream.rs): `play` **enqueues** PlayStream. Ok proves command acceptance, not that IAudioClient::Start already succeeded. Subsequent native startup failures are accepted-stream faults reported by AudioStatus, as the contract permits. The callback/queue handoff never treats this as synchronous rejection. Drop sends Terminate and joins.
- [CPAL WASAPI device](https://docs.rs/crate/cpal/0.18.1/source/src/host/wasapi/device.rs): generated WAVEFORMATEXTENSIBLE uses `KSAUDIO_SPEAKER_DIRECTOUT`. StreamConfig exposes channel count, rate and buffer size, not an ordered device speaker map. The default adapter therefore negotiates known mono/stereo; a multichannel-only device returns Unsupported. It does **not** infer Windows 7.1 order from the number 8.
- [CPAL CoreAudio lifecycle](https://docs.rs/crate/cpal/0.18.1/source/src/host/coreaudio/macos/mod.rs): native start/stop and teardown. macOS source was audited but not built or run in this environment.

The explicit offline Surround71 order is FL, FR, Center, LFE, BackLeft, BackRight, SideLeft, SideRight. Only FL/FR receive L/R. This is a declared test layout, not a claim about an actual Windows device. Unknown hardware layouts require additional native layout discovery or known-layout renegotiation. The existing contract is preserved.

## PCM, timing and bounds

Each fill receives 1..=max_pull_frames complete, interleaved stereo frames. Each chunk is cleared first. Larger native buffers are split without growing the scratch buffer. Bad native length or sample type yields whole-buffer silence, DeviceContract failure and an invalidated epoch. Nonfinite L/R become zero; mono uses 0.5L+0.5R, other known channels remain silent. Integer output clamps; unsigned silence is 32768. Supported types are f32, f64, i16, u16 and i32; unsupported native types are excluded during negotiation.

Default pull bound: 512 frames (10.667ms at 48kHz); maximum configurable bound: 4096. Conversion scratch is 2×max_pull_frames×4 bytes (4096 bytes by default). The native callback buffer stays borrowed. Kira's default three Frame buffers at 128 frames total 3072 bytes, plus fixed resource/command metadata. Kira sound capacity defaults to 64 and is limited to 256; unused resources have separate bounded reclamation capacity. Volume/stop use Kira's coalesced command slots, so full play capacity cannot block them. No new IO pipeline is implemented here.

DecodedSound requires an owned BudgetTicket covering input vector capacity plus converted Frame storage during construction; reserve before allocating input. Its conservative charge follows the Kira Sound into deferred retirement and is released once. A rejected play returns the original DecodedSound. Sound memory is capped at 8,388,608 stereo frames per input; the caller's pool may impose a smaller aggregate ceiling. For worker-produced PCM, bounded_pcm_queue accepts 1..=96,000 frames (8 bytes/frame); empty queue frames become silence and increment output shortage accounting. It consumes no content while empty. Device xruns count separately from the number of known missing frames; CPAL supplies no missing-frame count for its Xrun event. Counters reset on successful configure.

Clock snapshots use triple_buffer, with a Mutex only around the **control-side reader**. The callback never touches that Mutex. submitted_frames counts device frames supplied in the accepted stream, including explicit underrun silence. At callback entry the new block has not yet played: presented estimation uses prior submitted frames minus timestamp-derived latency, monotonically bounded by previous observations. It never equates the new submitted total with hardware consumption. CPAL playback-minus-callback timestamps provide latency; the host MonoTime anchor plus Instant elapsed identifies the observation time. Precision is Estimated with uncertainty=None, because CPAL cannot bound presentation error. Offline injected timings also stay Estimated. Suspend freezes the observation; Failed/Stopped observations are Unavailable. OutputEpoch changes on configure/start/fault so retained old observations must be discarded. Content position from Playback is a consumption position, not a hardware playback-completion guarantee.

## Reproducible checks

```sh
cargo test -p reel-media --locked
cargo test -p reel-media --no-default-features --locked
cargo clippy -p reel-media --all-targets --locked -- -D warnings
cargo clippy -p reel-media --no-default-features --all-targets --locked -- -D warnings
cargo fmt -p reel-media -- --check
cargo tree -p reel-media --no-default-features --edges normal
cargo check -p reel-media --all-targets --target x86_64-pc-windows-gnu --locked
crates/reel-media/tests/support/run_pcm_probe.sh
cargo run -p reel-media --example device_probe --locked
```

`tests/audio_contracts.rs` uses actual Kira StaticSound and Renderer, including two-track summation, stereo→8ch, mono, integer/float conversion, finite-value handling, malformed buffers, PCM shortages, freeze/resume, fault→stop→configure→new Kira and budget reclamation. `src/output/tests.rs` drives the **same lifecycle implementation** with injected build/play/pause/verify failures and an observable in-flight interval. The failed native operations in those unit fixtures are simulated output failures, not hardware measurements. Both tests of immediate native rejection assert identity of the original uncalled Box. The concurrent fixture deliberately blocks its fill to prove the fence; production fills must remain bounded.

`tests/support/pcm_probe.rs` is a standalone allocator observer compiled by its script, following D01's existing probe pattern. Production code retains unsafe_code=forbid. It runs the real Kira mixer continuously, including a finite Sound's deferred retirement, through the normal 8ch converter. The observer records allocation/reallocation/deallocation only on the calling PCM thread, excluding setup, teardown, control notification and unrelated threads. This proves the tested data path, not arbitrary third-party effects or every native driver internals. Source audit verifies that this path has no file IO or main-thread wait.

See `VERIFICATION.md` for the actual environment, command outputs and remaining device targets.
