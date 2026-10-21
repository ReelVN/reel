# reel-media: AudioOutput / reel-audio bridge (contract 1.1)

This workspace crate owns device output, playback activity and PCM retirement.
The mixer and sound renderer live in [reel-audio](../reel-audio/README.md).

## Review entry points

- `src/contracts.rs`: engine-owned AudioOutput, PcmFill, configuration, ordered channel map, status and clock snapshot types.
- `src/mixer.rs`: private reel-audio `Backend::setup((rate, signal, Renderer credit), buffer_size)` / `start(Renderer)` bridge. Fill calls the real Renderer through a whole deferred Drop shell, preserving linear `take_fill()`. `KiraMixer::new(format, limits, &service)` requires the host-owned cleanup service.
- `src/maintenance.rs`: `AudioReclaimService::new(ReclaimLimits, WakeSignal)`, status and explicit final-host `drain_and_join()`. It owns two preallocated rtrb queues and one non-RT worker; PCM touches only atomic flags. [Interface and credit proof](RECLAIM-DESIGN.md).
- `src/output.rs`: common lifecycle and callback used by OfflineOutput and CpalOutput. `DrivenOutput::start`, `Shared::enter/close_and_wait/open`, `CallGuard::drop`, and `DrivenOutput::stop` are the ownership review points.
- `src/desktop.rs`: actual `DeviceTrait::build_output_stream`, `StreamTrait::play/pause` calls and native error classification.
- `src/pcm.rs`: prezero, block splitting, finite sample sanitation, explicit mapping and integer saturation.

`reel-audio` follows the workspace version and provides host-driven mixing with
default features disabled; [source origin and licenses](../reel-audio/PROVENANCE.md)
remain documented. This crate alone owns CPAL **0.18.1** device output (optional
`cpal-output`, desktop targets, `realtime` promotion attempts). Other pinned
dependencies include atomic-arena **0.1.2**, rtrb **0.4.0**, triple_buffer **9.0.0**,
atomic_refcell **0.1.13**, and sync_wrapper **1.0.2**. Realtime scheduling denial is
counted and does not discard the mixer. No window, graphics renderer or DSL
dependency is introduced.

## Ownership and native API evidence

1. `configure` requires Stopped and negotiates a real format/layout pair. Mixer channels are always 2. Initialize reel-audio only after this returns the actual device sample rate.
2. `start` keeps the original PcmFill in its stack while verifying the device, building the native stream and calling native `play()`. The callback references an empty fixed FillSlot and a CLOSED entry gate. FillSlot uses AtomicRefCell<SyncWrapper<Option<PcmFill>>>; SyncWrapper permits only mutable access so PcmFill retains its original Send-only contract. Even a callback during build/play can only write silence. Any synchronous refusal returns the **same Box**, never invoked.
3. After native `play()` returns Ok, start inserts fill once in the control-owned slot, records ownership and opens entry. An asynchronous native error can instead latch FAULT; opening uses compare_exchange so it cannot clear that fault. The accepted fill remains owned until stop, even if never first invoked.
4. Only a successful 0→ACTIVE exchange permits a call. The callback then attempts one AtomicRefCell mutable borrow; it never retries, spins, or waits for that borrow. A conflict is a DeviceContract fault and whole-buffer silence. suspend/stop atomically add CLOSED and wait for ACTIVE to clear. The mutable slot borrow ends before CallGuard releases ACTIVE, when the entire native buffer finishes. This control-side wait is solely the required in-flight fence; the realtime thread never waits for the main thread, acquires a callback Mutex or participates in a command acknowledgement handshake.
5. Successful suspend waits for the active call, pauses the native stream and retains the same closure/Renderer. reel-audio's logical content position freezes because no Renderer calls occur. Resume verifies device identity, default format and support for the negotiated pair, then plays and reopens the same gate. Failure remains Failed with retained fill, until stop.
6. Stop closes entry, fences the active call and **takes/disposes the fill shell on its caller** before native pause/drop. reel-audio's shell moves its whole Option<Renderer> with a pre-reserved teardown credit; physical PCM destruction occurs on the service worker. Stopped means supply stopped, while tickets describe asynchronous residency. A pause failure still returns Stopped; native callback retention cannot recover the disposed fill. Native pause/stream Drop/in-flight fencing have no 0.5ms promise. Renderer ownership never relies on native callback destruction; device physical drain is also not guaranteed.
7. Each new native stream gets its own gate/fault latch. Late old-stream errors cannot close a replacement stream or change its epoch. Generic WakeSignal notification runs on a dedicated control notifier (2ms polling of a coalesced atomic flag), not in the realtime callback. Host code acknowledges WakeSignal before draining the shared queue.

Official versioned sources used for the interface audit:

- [Backend](../reel-audio/src/backend.rs), [Renderer](../reel-audio/src/backend/renderer.rs): owned Renderer and the real stereo process entry.
- [Resource storage](../reel-audio/src/backend/resources.rs): the guard checks producer credit **before DrainFilter.next()**; a full unused queue keeps finished Sound in its arena. Main-track play bypasses implicit bulk cleanup. Pure-move `try_take_unused_sound()` is called only after reserving an actual service queue chunk. `reclaim_unused(limit)` reports transferred_sounds; it runs no physical destructors. BudgetedSound delegates to real StaticSound and drops PCM before its ticket. Successful play also moves the original DecodedSound into an owned worker job; no accepted PCM alias can become a last reference on the preempted control caller. Rejection returns the unchanged unaccepted request.
- [CPAL traits](https://docs.rs/crate/cpal/0.18.1/source/src/traits.rs): build consumes a FnMut callback, returns a Stream, and play/pause return Result. There is no generic callback recovery API for a failed build and no public ordered speaker layout API.
- [CPAL ALSA](https://docs.rs/crate/cpal/0.18.1/source/src/host/alsa/mod.rs): `StreamTrait::play` releases its worker latch and can fail on ALSA start/unpause; `pause` can be unsupported or fail; `Stream::drop` wakes and joins the worker.
- [CPAL WASAPI stream](https://docs.rs/crate/cpal/0.18.1/source/src/host/wasapi/stream.rs): `play` **enqueues** PlayStream. Ok proves command acceptance, not that IAudioClient::Start already succeeded. Subsequent native startup failures are accepted-stream faults reported by AudioStatus, as the contract permits. The callback/queue handoff never treats this as synchronous rejection. Drop sends Terminate and joins.
- [CPAL WASAPI device](https://docs.rs/crate/cpal/0.18.1/source/src/host/wasapi/device.rs): generated WAVEFORMATEXTENSIBLE uses `KSAUDIO_SPEAKER_DIRECTOUT`. StreamConfig exposes channel count, rate and buffer size, not an ordered device speaker map. The Windows adapter requests stereo/mono client streams through WASAPI PCM conversion even when enumeration reports only the multichannel device default. The system performs the channel conversion. Other platforms require enumerated mono/stereo layouts. Physical speaker order remains a device-specific check.
- [AtomicRefCell source](https://docs.rs/crate/atomic_refcell/0.1.13/source/src/lib.rs): `AtomicBorrowRefMut::try_new` performs one compare_exchange, returning an error on conflict; no retry or blocking path. [SyncWrapper source](https://docs.rs/crate/sync_wrapper/1.0.2/source/src/lib.rs): Sync is sound without requiring the contained value to be Sync because access requires exclusive `&mut self`. Together they support the exact `FnMut + Send` contract without a callback Mutex or custom unsafe code.
- [CPAL CoreAudio lifecycle](https://docs.rs/crate/cpal/0.18.1/source/src/host/coreaudio/macos/mod.rs): native start/stop and an Arc<Mutex<StreamInner>> lifetime shared with device monitors. This is why outer Stream destruction alone is insufficient as a proof of synchronous fill disposal. The control-owned slot addresses it without changing the public contract. macOS source was audited but not built or run in this environment.

The explicit offline Surround71 order is FL, FR, Center, LFE, BackLeft, BackRight, SideLeft, SideRight. Only FL/FR receive L/R. This is a declared test layout, not a claim about an actual Windows device. Windows client-stream conversion does not establish this physical speaker order; native multichannel output on other platforms still requires a known layout.

## PCM, timing and bounds

Each fill receives 1..=max_pull_frames complete, interleaved stereo frames. Each chunk is cleared first. Larger native buffers are split without growing the scratch buffer. Bad native length or sample type yields whole-buffer silence, DeviceContract failure and an invalidated epoch. Nonfinite L/R become zero; mono uses 0.5L+0.5R, other known channels remain silent. Integer output clamps; unsigned silence is 32768. Supported types are f32, f64, i16, u16 and i32; unsupported native types are excluded during negotiation.

Default pull bound: 512 frames (10.667ms at 48kHz); maximum chunk size 4096, with larger native buffers split across chunks. Conversion scratch is 2×max_pull_frames×4 bytes (4096 by default); native buffers stay borrowed. reel-audio's three default 128-frame buffers total 3072 bytes plus fixed metadata. Sound capacity N defaults to 64; active/pending and unused each have N slots. Reclaim transfers at most min(limit,N), play runs no old-resource destructors, and volume/stop use independent coalesced command slots. Service limits default to G=4 generations and R=64 queued normal jobs: R+1 queued/in-flight jobs, 2G reserved teardown credits and G live/old generations. Hosts choose sound, internal mixer-buffer, activity and reclamation capacities without additional fixed maxima. Capacities must be nonzero and storage representable; queue metadata is preallocated before audio starts. Each job/owner retains its generation until complete. PCM residency is accounted by the actual BudgetPools throughout physical retirement. No new IO pipeline is introduced.

DecodedSound requires an owned BudgetTicket covering input vector capacity plus converted Frame storage during construction; reserve before allocating input. Its conservative charge follows the reel-audio Sound into deferred retirement and is released once. A rejected play returns the original DecodedSound. Sound length is limited by its real memory reservation, with no separate frame-count or duration ceiling. For worker-produced PCM, bounded_pcm_queue preallocates the caller's nonzero capacity (8 bytes/frame); empty queue frames become silence and increment output shortage accounting. It consumes no content while empty. Device xruns count separately from the number of known missing frames; CPAL supplies no missing-frame count for its Xrun event. Counters reset on successful configure.

At every normal control tick, acknowledge the host wake before draining sources and call `reclaim_unused(limit)`. Before new decode allocation, `reserve_decode_budget` attempts transfer then immediately reserves or returns Busy; it never waits for budget. Inspect progress even on Busy. Full/contended destination leaves Sound in reel-audio; quota exhaustion requests another turn. Empty observation is not global residency completion. Worker job counts describe destruction of those jobs; accepted aliases may still retain PCM, so the real pool remains authoritative. Completion wakes the same scheduler after destruction, including while UI sleeps. Decoding/conversion is producer work outside the control transfer deadline.

The host service outlives mixers. Each generation reserves Renderer/Manager credits before construction; ordinary work cannot consume them, and unavailable generation credit returns Busy. Fill Drop only moves Renderer; Mixer Drop only moves Manager, including an untaken fill whose worker-side Drop submits its own Renderer credit. Neither joins. Final host exit drops all control owners then explicitly calls `drain_and_join`; outstanding control owners return Busy. Service Drop safely detaches a draining worker. The complete reserved byte charge stays live until physical PCM destruction on the worker. See [the ownership and measurement boundaries](BACKGROUND-RECLAIM.md).

Clock snapshots use triple_buffer, with a Mutex only around the **control-side reader**. The callback never touches it. `generated_frames` counts software-produced frames including silence, for diagnostics. CPAL declares Unavailable: its callback has no write acknowledgement, and ALSA writei Err(EAGAIN) with frames_written==0 silently drops the generated block; actual Ok(0) retries without progress. CPAL latency/timestamps stay diagnostic. Under any Unavailable precision, submitted/presented numeric zeros are invalid, and both `submitted_frames()` and `presented_frames()` return None. They cannot anchor MediaPTS.

The controlled offline sink declares Estimated capability and supports injected timing: it guarantees submission to its sink, then estimates presentation from prior blocks minus the injected latency. The new block is never equated with presentation. A future native driver can opt into that path only after proving the submission/timing guarantees. Suspend freezes observations. Native Xrun closes entry, makes the output Failed/ReconfigureRequired, invalidates OutputEpoch and software counts, and requires control-side stop/rebuild. Failed/Stopped observations are Unavailable; every epoch change invalidates retained observations and media mappings. Playback position describes content consumption, not hardware completion.

## Reproducible checks

```sh
cargo test -p reel-media --locked
cargo test -p reel-media --no-default-features --locked
cargo clippy -p reel-media --all-targets --locked -- -D warnings
cargo clippy -p reel-media --no-default-features --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo tree -p reel-media --no-default-features --edges normal
cargo check -p reel-media --all-targets --target x86_64-pc-windows-gnu --locked
cargo run --locked -p reel-dev -- audio-regression
cargo run --locked -p reel-dev -- audio-regression --release
cargo run --locked -p reel-dev -- audio-probe
cargo run --locked -p reel-media --example device_probe
```

`tests/audio_contracts.rs` uses actual reel-audio StaticSound and Renderer, including two-track summation, stereo→8ch, mono, integer/float conversion, finite-value handling, malformed buffers, PCM shortages, freeze/resume, fault→stop→configure→new reel-audio and budget reclamation. `src/output/tests.rs` drives the **same lifecycle implementation** with injected build/play/pause/verify failures and an observable in-flight interval. The failed native operations in those unit fixtures are simulated output failures, not hardware measurements. Both tests of immediate native rejection assert identity of the original uncalled Box. A retained-native-callback fixture proves immediate control-side fill disposal even when native pause fails and the callback object survives stop. The concurrent fixture deliberately blocks its fill to prove the fence; production fills must remain bounded.

`audio-probe` builds the production crate in debug/release and selects its exact libraries from Cargo JSON. It compiles `tests/support/pcm_probe.rs` and `reclaim_probe.rs`; production code retains unsafe_code=forbid. The PCM probe runs the real reel-audio mixer continuously, including a finite Sound's deferred retirement, through the normal 8ch converter. The observer records allocation/reallocation/deallocation only on the calling PCM thread, excluding setup, teardown, control notification and unrelated threads. The reclaim probe observes actual System::dealloc on the service worker and asserts that the pool remains charged immediately before and after the physical free, then reaches zero after job completion. These checks cover this data path, not arbitrary effects or every native driver's internals.

Generated transcripts and measurements belong under ignored `target/reel-artifacts/`.
Audio library tests use the root workspace and Cargo.lock. See
[BACKGROUND-RECLAIM.md](BACKGROUND-RECLAIM.md), [RETIREMENT.md](RETIREMENT.md) and
[VERIFICATION.md](VERIFICATION.md) for contracts, current commands and fixed
historical evidence links.

## Logical audio activities

AudioActivityService runs over the reel-audio bridge. Requests own a ResourceVersion, MediaEpoch, group metadata and DecodedSound.
Requests use a registered AudioOwner carrying the complete Scope and a
service-local admission capability. Fixed ledger slots reserve terminal capacity before play; refusals return the owned request. QueueLimits charges slot storage and asset-key capacity, while existing tickets retain PCM charges. IDs are unique within a service lifetime; the host must allocate a fresh epoch when replacing that service. Group is metadata only, and looping follows the supplied DecodedSound.

play/stop/set_volume_db/snapshot operate on logical activity identities. Completed observes content consumption through Playback; it does not certify hardware presentation or released PCM. Stopped settles a control command; AudioOutput still owns the supply fence and the reclaim service still owns physical destruction. This low-level terminal must not silently become the final creator media-wait completion contract.

Before cancel_owner, revoke business waits and prevent new plays for that complete Scope. Continue its bounded cursor until finished. maintain/drain charge visited slots, including holes; caller output vectors do not grow. A full destination preserves deliverable records while still charging and advancing past them to release later cancelled/stale records. After a complete scan, consumer backpressure alone does not self-wake; the consumer calls drain again after capacity becomes available. Cancelled and records whose exact admitted (MediaId, MediaEpoch, Scope) no longer identify a live wait are disposed without business delivery. Delayed results require another live-wait check. Output failure never invalidates an already-settled logical result merely because the current admission epoch changed. Completed/Stopped/Failed retain their original identity; fail_output advances the admission epoch once and settles remaining activities as Failed under their original identities. It is not successful device reconstruction. The host fences/stops the output separately.

Share a WakeSignal across output, this ledger and AudioReclaimService. Acknowledge before draining all sources and recheck pending before sleeping. Partial scans request an immediate next turn. While unsettled activities remain, poll_after recommends a 10 ms control-only timer because Playback has no per-handle completion subscription. No unsettled activity means no periodic timer from this API. This polling does not request layout or presentation and does not guarantee completion latency under overload.

AudioActivitySnapshot exposes borrowed low-level inspection data. The player and
SessionVm compose full save descriptors, per-activity controls and resource
restoration above this service. Hardware completion, position-preserving device
reconstruction and the control-side timing targets need separate device checks.
Original review results remain in the [fixed activity record](https://github.com/jhq223/reel/blob/f916ffedceb91465633fd549e4db1084c070edf3/crates/reel-media/README.md).

## Physical owner retirement

Register the complete core Scope with `register_owner`, then pass the resulting
AudioOwner in AudioActivityRequest.owner. A foreign-service or closed capability
is rejected before play. Existing registrations for the same Scope return the
same capability, including when closed. After every capability, resource and
receipt for that registration disappears, registering that Scope creates a new
registration; session hosts must not recycle a retired Scope as the same owner.

First revoke business waits, then call `retire_owner(&owner, limit)`. It closes
admission before a persistent, slot-budgeted cancellation scan. Repeat while the
returned progress requests another turn. This progress describes cancellation,
not physical completion. Continue ledger maintenance/drain and the existing
worker reclamation. `AudioOwner::is_retired()` requires closed admission and
zero resource and descriptor obligations.

`retirement(MediaId)` obtains an AudioActivityRetirement receipt for an admitted
activity while its evidence still exists; obtain it before draining if needed.
It exposes admitted ID/epoch/full Scope plus resources_released,
descriptor_released and is_retired. A logical Completed/Stopped/Failed event
does not set either physical flag. Accepted PCM aliases share a Residency:
all actual PCM references drop first, then the last BudgetTicket, then the
resource-release marker. Entry Playback and owned resource key drop before its
descriptor marker. Accepted large PCM destruction stays on the existing worker,
not on the PCM callback. Failed admission returns an unaccepted request without
leaving a published receipt or owner obligation.

Owner registrations and activity evidence are separately bounded by max_requests.
Cloned receipts share one slot, but retained completed receipts still prevent
slot reuse; discard receipts no longer needed. Evidence storage is included in
QueueLimits accounting. Atomics use pointer-width counters for 32-bit targets.
Receipts remain queryable after service teardown, which closes all owners.
A taken fill can still retain sound resources; suspended output can keep
retirement pending until its stop/fence and normal worker cleanup finish.
No elapsed-time guarantee or hardware-presentation assertion follows.

The [fixed retirement verification record](https://github.com/jhq223/reel/blob/f916ffedceb91465633fd549e4db1084c070edf3/crates/reel-media/README.md)
preserves the original allocator and lifecycle checks. The current player connects
these services through `reel-player/src/media.rs`; `SessionVm` exposes the matching
script audio activities and snapshot resource gates. Video demux/decoding lives in
`src/video.rs`, with bounded PCM output connected to the same mixer. Use
[VERIFICATION.md](VERIFICATION.md) for current commands and device boundaries.

Automatic audio preparation uses the existing Kira queue/transport for immutable
PCM and worker-backed frame windows. The player host policy defaults to 8 MiB
stereo-f32 static PCM and a 16384-frame stream window. `Assets::stream` performs
range reads; long encoded files are not loaded whole. Missing samples produce
silence and freeze content position. Seeking, mixed-rate queues, looping and
restoration keep one logical activity. Stream workers poll at 1 ms during refill
and 8 ms while the requested position is unchanged; paused/prepared sources keep
only their current bounded window. These intervals are worker delays, not callback
waits. Each streaming source owns one decoder thread with an explicitly requested
512 KiB native stack; logical source capacity bounds their count (default 64, up
to 32 MiB requested stack reservation). OS thread/stack and decoder bookkeeping
remain outside the PCM pool. A stopped source cancels IO, and its budget survives until decoder teardown.

The PCM budget covers the window, 64 KiB reader buffer, current encoded packet,
codec output sample capacity and normalized conversion storage, retaining a
high-water reservation while the codec can retain samples. Symphonia format/codec
bookkeeping and allocation-before-return are not enclosed by that budget: this is
not a hard bound on arbitrary decoder input. A complete hard decoder budget needs
budget-aware allocations in Symphonia's format packet readers and audio-buffer
resize/allocation paths.
Reproduce the full unmodified long-track check with `REEL_LONG_AUDIO` set to an
external full track and `cargo test --locked --offline -p reel-player --lib
external_long_audio_streams -- --ignored --nocapture`. It covers startup under a
2 MiB PCM budget, uncached seeking, loop wraps, paused request restoration and
zero residency after shutdown. `cargo run --locked --offline -p reel-dev --
audio-probe` also observes the streaming loop/seek/stop callback in debug/release.
