# Issue #10: bounded Kira retirement and clock evidence

Recorded 2026-10-08, Rust 1.96.0, Debian 13 x86_64 KVM container. D01 dependency:
`issue-1-core-contracts` at `c03226a59a9dd4f8b448d4eea82262a1d6e57ef8`.
Initial media implementation: `71d9d56e009a3891ad1f6e439f3a7b15db1660ea`.
This record supersedes the initial dependency/clock statements in
[VERIFICATION.md](VERIFICATION.md). Integration-owner acceptance remains separate.

## Public interfaces and ownership

`AudioOutput` keeps configure/start/suspend/resume/stop and the exact Send-only
`PcmFill`. A synchronous start refusal returns the identical, uncalled Box. Until
native build/play succeeds, fill stays on the start caller's stack; native callbacks
see an empty slot and closed gate. Accepted fill moves once into a control-owned
slot. Suspend closes entry and fences an in-flight call before native pause; resume
verifies format and reopens that same slot. Stop closes/fences entry, destroys fill
on its caller, then pauses/destroys the native stream. A native pause error or
retained callback object cannot cause a later fill call. The fence waits only on
the control side. PCM uses one gate CAS and one fallible mutable borrow, with no
callback Mutex, allocation, file IO or wait for the main thread.

The actual Kira bridge implements `Backend::setup((rate, signal), buffer_size)`
and `start(Renderer)`; fill calls real `Renderer::on_start_processing()` and
`Renderer::process(out, 2)`. Construction now takes the existing host wake source:

```rust,ignore
KiraMixer::new(&format, limits, maintenance_wake)
mixer.reclaim_unused(resource_limit) // ReclaimProgress
mixer.reserve_decode_budget(&pool, bytes, resource_limit)
// -> (ReclaimProgress, Result<BudgetTicket, RejectReason>)
```

At every normal control tick, acknowledge WakeSignal before draining all sources,
then reclaim with an explicit resource quota. Before allocating/decoding new PCM,
use `reserve_decode_budget`, which reclaims before reserving a ticket; inspect
progress even on Busy. Quota exhaustion sets `needs_maintenance=true`, including
limit=0, and requests another turn through the same wake source. An empty observed
queue returns false without claiming all active/in-flight residency ended. Grant
a positive quota to make progress; do not spin a quota loop inside one tick.

Only atomic observation/pending flags are touched by PCM. Observation is made by
real Sound::finished(); notification is published **after** Kira's retirement
publication phase. A dedicated control notifier checks the coalesced flag every
2ms and invokes the generic WakeSignal. This closes the early-empty-drain race
and schedules maintenance while the UI sleeps, including a further turn after
quota exhaustion when there are no more PCM calls. It is a wake mechanism, not
a 2ms scheduling guarantee. Tests wait for that existing wake and acknowledge it.

## Minimal upstream modification and bounds

The approved [Kira 0.12.5 fork](../../third_party/kira/REEL-FORK.md) preserves the
published version, manifests, registry hash, exact VCS commit, original file
hashes, MIT/Apache licenses and a reversible two-file patch. The published crate
omitted licenses; the copies are from its exact VCS commit's repository root,
with URLs/hashes retained. `atomic-arena = 0.1.2` is an unchanged ordinary locked
dependency. No other ecosystem source is imported.

`backend/resources.rs` checks unused-producer credit **before** `DrainFilter.next()`
can free an arena slot. Full unused queues keep finished Sound in the arena and
apply backpressure. `track/main/handle.rs` exposes explicit bounded reclaim and
uses insertion without implicit old-Sound cleanup. Reel's private manager has
zero capacities for other upstream resources; their original APIs are outside
this bridge. Main-track play destroys zero previously retired resources.

For sound capacity N (default 64, allowed 1..=256), active/pending sounds share N
arena reservations; retirement has N separate bounded queue slots. Reclaim runs
at most min(limit, N) destructors. No RT drop is used to make room. All resident
PCM retains its BudgetTicket, including stopped arena sounds and unused-queue
sounds. BudgetedSound drops real Sound/PCM before its ticket by field order;
its private frame Arc cannot escape through a Weak reference. Root workspace
members remain `crates/*`; excluding `third_party/kira` prevents Cargo from adding
the path dependency as an engine workspace member. Root Cargo.lock changes only
Kira's source/checksum entries. Other engine crates and creator guides are unchanged.

## Deterministic regressions and actual PCM

[Captured regression output](tests/evidence/regressions.txt) and
[fork regression output](tests/evidence/kira-retirement.txt) establish these behaviors:

- Real StaticSound A `[0.1,-0.2]` finishes; a test-only hook pauses after its arena
  slot is freed but before unused publication. Control plays B `[0.3,-0.4]`, then
  A publishes and B finishes. On the next full-queue callback B stays in its arena
  slot; no second retirement attempt/drop occurs. Quota 0 frees none, quota 1 frees
  one, and a successful play(C) cannot secretly reclaim queued B. Scheduling
  barriers and module/call declarations are `#[cfg(test)]` only. The prior
  [unpatched scheduling run](tests/evidence/upstream-retirement-panic.txt) records
  the actual upstream `unused resource producer is full` panic.
- The same mixer/fill progresses A→B with either max_requests=1 or a pool with
  only 1024 bytes (one 64-frame sound's construction charge). Stopped A stays
  charged; publication wakes control; pre-reservation reclaim makes B's ticket
  available. Reaching quota wakes a further empty-check tick without another
  audio callback. There is no mixer teardown to obtain the new ticket.
- Real Kira generates 512 frames before an injected native EPIPE/Xrun. The
  default-feature test invokes the actual CPAL error dispatcher. The gate closes,
  epoch 4→5, status becomes Failed/ReconfigureRequired, old observations become
  invalid, the next callback is silent with fill calls unchanged, and software
  counters stop advancing. Stop/reconfigure/new Kira yields a fresh epoch.
- A separately injected ALSA silent EAGAIN discard has no Xrun notification.
  Even with the next latency=0, generated=1024 is diagnostic, submitted/presented
  remain invalid, and `presented_frames()` is None. This fixture validates the
  capability boundary; it is not a hardware write-failure injection.

Existing actual-Kira conversion/lifecycle checks produce two-track stereo sum
`[0.5,-0.3,0,0,0,0,0,0]` at declared 8ch/48kHz/f32, exercise block splitting,
mono, f32/f64/i16/u16/i32, NaN/Infinity, partial fill silence, malformed frames,
saturation, suspend/resume freeze, original-Box start refusal and retained native
callbacks after stop. Surround71 order is explicitly FL/FR/Center/LFE/BackL/BackR/
SideL/SideR. This proves a declared layout converter, not an actual Windows mask.

## Clock capability

`AudioCaps::clock` declares `AudioClockCapability::{Unavailable, Estimated}`;
`AudioClockObservation::generated_frames` is epoch-scoped software diagnostics.
CPAL always declares Unavailable. Its callback precedes native writing and lacks
write acknowledgement. In the actual pinned [ALSA implementation](https://docs.rs/crate/cpal/0.18.1/source/src/host/alsa/mod.rs),
**writei Err(EAGAIN) when frames_written==0 returns Ok and drops the generated
block**; actual **writei Ok(0)** instead retries without progress. They are distinct
branches. Callback counts and timestamp latency prove neither acceptance nor
presentation. Under Unavailable, submitted/presented zeros are invalid sentinels,
precision is Unavailable, uncertainty is None, and `presented_frames()` returns
None. MediaPTS must not use those numbers. OutputEpoch changes invalidate old
media mappings and observations, including software counts after a native fault.

The controlled offline sink explicitly guarantees submission and permits injected
Estimated timing based on preceding blocks. No hardware estimate is inferred
from that test. A future confirmed native driver has this capability adaptation
point; no Windows device-clock implementation is included here.

## Allocator, indivisible destruction and performance

[Captured probes](tests/evidence/audio-probes.txt) run shipping production code
without scheduling hooks. Both debug/release use actual Kira plus the normal
8ch/i16 converter for 4096×512 frames. PCM-thread allocations/reallocations/
deallocations are all 0, including a retired A/full-queue stopped B case (19 PCM
calls, then explicit control reclaim 1+1). Real overload produces
`[32767,-32768,0,0,0,0,0,0]`. The standalone observer forwards allocation operations
to System and observes only its enabled thread; production crates retain
unsafe_code=forbid. This does not cover arbitrary future effects or every native
driver implementation.

| Profile | PCM p95 | PCM p99 | Maximum observed PCM call | 512-frame deadline |
| --- | ---: | ---: | ---: | ---: |
| Debug | 368.888µs | 523.052µs | 3335.097µs | 10666.667µs |
| Release | 32.098µs | 54.853µs | 287.815µs | 10666.667µs |

The reclaim probe keeps the same real mixer, allocates/touches PCM only after
reservation, stops the actual Sound, retires it, and times only explicit quota=1
reclaim. Sixteen samples are recorded per size/profile. Weak liveness assertions
in tests are not physical allocation proofs: a Weak of Arc<[Frame]> can retain
the allocation after strong ownership ends. The engine-budget test drops its
Weak before reclaim. This independent probe retains no Weak and records actual
System::dealloc sizes; maximum PCM's 67,108,880-byte Arc layout is forwarded to
System before budget usage becomes zero.

| Real PCM | Construction ticket | Debug median / max free | Release median / max free |
| --- | ---: | ---: | ---: |
| 64 frames / 512 bytes | 1024 bytes | 0.911 / 3.725µs | 0.081 / 0.841µs |
| 48,000 frames / 384,000 bytes | 768,000 bytes | 19.409 / 23.746µs | 12.549 / 48.193µs |
| 8,388,608 frames / 64MiB | 128MiB | 1930.895 / 2366.473µs | 2160.502 / 3025.448µs |

**The maximum supported one-Sound destruction exceeds the architecture's 0.5ms
control-reclaim target.** Resource quotas bound counts, not the time of one
indivisible destructor. The 3.025ms measured maximum includes observer overhead
and host scheduling noise; it is not a worst-case execution-time guarantee or a
device measurement. A stricter control-tick target still needs an owner-approved
smaller PCM bound or different control-side destruction strategy. It cannot be
met by moving PCM destruction to RT or by claiming quota=1 makes it preemptible.

Default conversion scratch is 4096 bytes; Kira's three 128-frame buffers total
3072 bytes plus fixed metadata. Probe PCM construction peak is 1,537,024 bytes.
Optional PCM queue sample storage is bounded by 96,000×8=768,000 bytes. Aggregate
decoded residency remains bounded by the caller's BudgetPool; tickets cover
PCM/input overlap, not every allocator header or all engine metadata.

## Commands and remaining targets

The environment setup is retained in historical VERIFICATION.md. The following
commands were actually run successfully (default/offline builds use the pinned
lock; probe scripts also use --locked):

```sh
cargo test -p reel-media --locked --offline -- --nocapture
cargo test -p reel-media --no-default-features --locked --offline -- --nocapture
cargo clippy -p reel-media --all-targets --locked --offline -- -D warnings
cargo clippy -p reel-media --no-default-features --all-targets --locked --offline -- -D warnings
cargo fmt -p reel-media -- --check
cargo check -p reel-media --all-targets --target x86_64-pc-windows-gnu --locked --offline
cargo tree -p reel-media --no-default-features --locked --offline --edges normal
python3 crates/reel-media/tests/support/verify_kira_fork.py
crates/reel-media/tests/support/run_kira_regression.sh
crates/reel-media/tests/support/run_pcm_probe.sh
```

The fork test script copies the exact fork into an isolated temporary crate and
uses committed `kira-test.lock` for upstream dev dependencies. Those ordinary
test dependencies do not change production workspace membership or features.
Provenance verification reverses the two-file patch and verifies all 126 original
file hashes plus the exact-commit license hashes. Feature-off normal dependency
tree has no CPAL, ALSA, window/render, decoder or VM dependency.

The physical command `cargo run -p reel-media --example device_probe --locked
--offline` exits 2: ALSA `cannot find card '0'` / `Unknown PCM default`, followed
by device_unavailable. `/dev/snd` is absent. With temporary ALSA `pcm.!default {
type null }` and ALSA_CONFIG_PATH, the same command runs the actual CPAL worker
and real Kira, reports clock Unavailable despite generated_frames=38257 and
latency=0, and stops with native pause errno 77. After stop, fill_calls=38267 and
post_stop_fill_calls=0. [Both device transcripts](tests/evidence/device-probes.txt)
are retained. This is a virtual null sink, not hardware playback or clock proof.

Remaining: integration-owner review/rerun; physical Windows x64/N100 8ch/48kHz
speaker mask/order, audible output, xruns and tails; actual Linux/macOS devices,
hotplug, native startup failure and hardware suspend/resume. Windows is only a
cross-target cargo check; macOS was source-reviewed, not built/run. CPAL lacks
a public ordered device layout, so this adapter negotiates known mono/stereo or
rejects unknown multichannel-only layouts. No MediaPTS sync service, Windows
device clock, video, decoder, DSL, full player, guide expansion, Actions, main
modification or remote merge is included. The existing PR remains draft.
