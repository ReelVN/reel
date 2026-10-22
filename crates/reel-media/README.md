# Reel media (v1)

`reel-media` owns device output, audio activities, PCM preparation and physical
retirement. [`reel-audio`](../reel-audio/README.md) supplies the host-driven mixer.
The [architecture](../../docs/engine-architecture.md) defines session integration.

| Source | Responsibility |
| --- | --- |
| [contracts](src/contracts.rs) | AudioOutput, PcmFill, format, status and clocks |
| [output](src/output.rs) | Shared callback gate, lifecycle and fill ownership |
| [desktop](src/desktop.rs) | Optional CPAL device creation and play/pause |
| [pcm](src/pcm.rs) | Bounded conversion, channel mapping and silence |
| [mixer](src/mixer.rs) | Reel-audio Renderer and decoded-sound ownership |
| [maintenance](src/maintenance.rs) | Host-owned background reclamation service |
| [video](src/video.rs) | Demux/decoding and bounded audio output |

## Output lifecycle

Configure a stopped output to obtain its actual format before constructing the
mixer. Start retains the original PcmFill while verifying/building/playing the
stream. The native callback initially has an empty fill slot and closed gate;
synchronous rejection returns the same uncalled Box. Successful start publishes
fill ownership once and opens entry unless a fault has already latched.

Each callback enters through one atomic gate and a nonblocking mutable borrow.
Conflict produces silence and a contract fault. Stop/suspend closes entry and
fences any active call on the control side. Suspend retains the closure/Renderer;
resume verifies the negotiated device configuration before reopening entry.
Stop takes the fill shell before native pause/drop. Its Renderer transfers to
the background service; the Stopped result means supply has ended.

Each stream has its own fault/epoch state. Callback work uses preallocated buffers
and coalesced atomics; generic host notification runs on a control notifier.
Native pause, stream destruction and in-flight fencing use their actual driver
behavior.

## PCM and clocks

Fill receives bounded interleaved stereo frames. Buffers are cleared before fill;
larger native requests split across the configured pull bound. Nonfinite samples
become zero, integer conversion clamps, and mono averages L/R. Other channels
follow the explicitly negotiated layout. Windows requests a mono/stereo WASAPI
client stream and uses system conversion; physical speaker order requires a
device-specific layout.

CPAL clock precision is `Unavailable`: generated frames and latency are
diagnostics; submitted/presented frame queries return `None`. Controlled offline
output guarantees sink submission and may expose Estimated presentation. Media
content position tracks consumption separately. Epoch changes invalidate clock
mappings; native xrun/fault requires stop and reconfiguration.

## Reclamation and budgets

The host owns `AudioReclaimService` longer than its mixers. Before construction,
each live generation reserves Manager and Renderer teardown credits. Normal
reclamation reserves an actual destination chunk before taking a retired Sound;
full or contended transfer leaves ownership in the mixer. `reclaim_unused(limit)`
reports transferred sounds. `reserve_decode_budget` attempts transfer and then
reserves or returns Busy; worker completion wakes the host to retry.

For R ordinary queue slots and G generations, bounds are R+1 ordinary queued/
in-flight jobs and 2G teardown credits. A generation with sound capacity N can
retain N active/pending and N unused sounds. Queue metadata is preallocated;
PCM retains its real byte reservations through physical destruction. Accepted
DecodedSound ownership also transfers to the worker. Rejection returns the
original unaccepted request. Native Sound/PCM drops before its BudgetTicket.

Fill/Mixer Drop transfers whole shells using reserved credits. Final host shutdown
drops control owners then calls `drain_and_join`; outstanding owners report Busy.
Service Drop requests detached draining. Job/wake faults close new admission
while already-owned work and reserved teardown continue draining.

## Activities and owner retirement

`AudioActivityService` admits requests under a full-Scope AudioOwner capability
and reserves terminal capacity before play. Logical completion retains the
admitted MediaId/MediaEpoch/Scope. The host revokes business waits before bounded
owner retirement, then drives maintenance and physical reclamation. Retained
receipts separately report resource and descriptor release; owner reuse waits
for all obligations. Completed logical playback and physical PCM release have
separate terminal conditions.

Acknowledge the shared WakeSignal before draining sources and recheck pending
work before sleep. Partial scans schedule another turn. Consumer backpressure
retries after capacity becomes available. Unsettled playback uses the service's
control-only polling interval; settled/idle activities permit sleep.

Static PCM and streaming windows share transport, seeking, resampling, looping
and retirement. Streams use range IO and bounded PCM/reader/packet/sample
reservations. Decoder workers own their stack and bookkeeping separately;
Symphonia's internal allocations retain their library behavior. Source limits
and worker policy are documented beside their implementations.

## Checks

```sh
cargo test --locked -p reel-media
cargo test --locked -p reel-media --no-default-features
cargo run --locked -p reel-dev -- audio-regression
cargo run --locked -p reel-dev -- audio-probe
cargo run --locked -p reel-media --example device_probe
```

Offline tests cover actual mixing, lifecycle failure injection and ownership.
Allocator probes observe callback allocations and physical worker release.
`device_probe` requires the target's native output device. Generated measurements
belong under ignored `target/reel-artifacts/`; fixture sources are documented
[beside the media inputs](tests/fixtures/README.md).
