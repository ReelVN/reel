# Issue #10: host-owned background PCM destruction

Implementation commit: `18eb52350f8282af96939cbcfe461a58fad155c8`; preserves the
owner's architecture commit `5e142629aa3ddb97bbaa602075c6d54e9e9140f4` and accepted
D01 `c03226a59a9dd4f8b448d4eea82262a1d6e57ef8`. Rust 1.96.0, Linux x86_64/KVM,
AMD EPYC 9V74; current environment reports nproc=5 and cgroup CPU quota 4.
Measurements are container observations, not N100/hardware guarantees.

## Interfaces and ownership

[RECLAIM-DESIGN.md](RECLAIM-DESIGN.md) contains the concrete credit proof.
The host constructs `AudioReclaimService::new(ReclaimLimits, WakeSignal)` and
keeps it independent of mixers. `KiraMixer::new(format, limits, &service)` reserves
one generation plus two unique Renderer/Manager teardown credits before Kira
construction. Default limits are G=4 generations and R=64 queued normal jobs;
allowed maxima are G=64/R=256. Old generations remain counted while any accepted
job or internal owner remains; exhausted credit returns Busy without construction.

Normal producers use a short control-only try_lock and reserve an actual safe
rtrb `write_chunk_uninit(1)` **before** Kira's pure-move
`try_take_unused_sound()`. Full/contended destination does not pop/drop Sound.
The reserved chunk commits `once(job)` without a fallible send afterward. Worker
destruction holds neither producer mutex. PCM only publishes atomic flags.

`reclaim_unused(limit)` reports **transferred_sounds**, bounded by min(limit,N),
not completed destruction. Quota/full requests maintenance. `reserve_decode_budget`
attempts transfer then immediately returns a ticket or Busy: control and RT never
wait for budget completion. The host acknowledges its wake before draining all
sources; worker completion wakes it to retry, including while UI sleeps.

Successful play also reserves normal queue credit before acceptance and transfers
the original owned DecodedSound to a worker job. Kira's cloned Sound data alone
was insufficient: a preempted caller could otherwise become the last PCM owner.
DecodedSound has no public Clone, Arc/Weak or native-data access. Rejected and
unaccepted requests remain unchanged caller ownership. Decoding/conversion belongs
to the producer and is outside the measured control transfer deadline.

The fill captures a whole deferred Drop shell containing Option<Renderer>, and
Mixer owns Option<AudioManager> in another shell. Both Drop paths reserve their
guaranteed teardown chunk before taking the object. The separate queue has 2G
slots; every queued/in-flight/unsubmitted object owns a unique credit, so an
outside credit proves space. Ordinary work cannot consume it. No per-mixer join
or large PCM destruction occurs on the control path. An untaken fill already
inside a queued Manager is background owned; its eventual worker-side Drop
submits its reserved Renderer credit. `control_owners` distinguishes this from
an actual host-held fill even while `unsubmitted_teardowns` is nonzero.

AudioOutput.stop retains the synchronous gate/fence and disposes the fill shell.
Stopped means no more supply; real residency remains ticketed asynchronously.
Native pause, stream destruction and in-flight fences have no 0.5ms promise.
Construction abort uses the same native-owner shells. Worker job/wake failures
are caught/reported, new acceptance refuses, and existing consumers/teardown
destinations stay alive. Final host exit drops all control owners and explicitly
calls blocking `drain_and_join`; live control owners return Busy. Service Drop
detaches a draining worker rather than joining or discarding queued PCM.

BudgetedSound drops native Sound/PCM before its ticket, and DecodedSound drops
data before its ticket. Every accepted internal alias is worker owned. Job
completion counters count those destructors; another accepted job may still
retain the same PCM. The real BudgetPool is authoritative and stays charged
until its last PCM-owning ticket reference ends. No early refund is introduced.

## Bounds and deterministic evidence

Queue/in-flight limits are R+1 normal jobs, 2G total teardown credits and G total
live/old generations. Kira has N active/pending reservations plus N unused slots,
N<=256. A conservative unique PCM bound is `(2*256*G + R+1)*64MiB`, before the
caller's tighter real pools; accepted-data aliases do not duplicate PCM. Inline
queue slots are preallocated (Renderer variant is 1744 bytes on this target,
plus envelope credits/queue metadata); no boxing allocation is added at teardown.
The 8,388,608-frame / 64MiB capability and conservative 128MiB construction ticket
remain unchanged. Default scratch=4096 bytes; default three Kira buffers=3072
bytes plus fixed metadata. No new dependency or core change is needed.

[Regression transcript](tests/evidence/background-regressions.txt) proves:

- Same real mixer/fill A→B with either one ticket or one-sound byte capacity.
  Transfer=1 remains Busy while a test-only worker gate holds A; only physical
  destruction and completion wake allow B's new ticket/actual `[0.3,-0.4]` PCM.
- R=1: one in-flight plus one queued job; a third retired Sound stays in Kira's
  unused queue and play returns its unchanged request. Quota 0 transfers none.
  Ordinary saturation cannot consume Renderer/Manager exit credit. Old generation
  blocks rebuild until completion; a later untaken-fill generation drains cleanly.
- Control pauses after real Kira play acceptance. Actual Renderer finishes and
  is destroyed by worker first; the original request stays charged, then moves
  as the last PCM reference to its reserved worker slot when control resumes.
- Construction abort transfers both native owners. Injected worker failure
  unwinds an actual Sound on worker, reports fault, refuses new generations and
  still drains reserved teardown. Host join reports that error after emptying;
  service Drop before late mixer Drop retains the consumers and drains safely.
- Native Xrun invalidation and silent EAGAIN protection remain. Under any
  Unavailable precision, numeric submitted/presented values are invalid zero
  placeholders and **both queries return None**; generated diagnostics remain
  separate. The no-latency controlled-driver case is also formally covered.

Scheduling hooks are exclusively #[cfg(test)]. [Kira's original capacity-1
interleaving regression](tests/evidence/background-kira.txt) and shipping probes
use real StaticSound/Renderer. Production builds contain no hook/barrier feature.

## Separate main work from worker physical release

[Raw probe output](tests/evidence/background-audio-probes.txt) comes from shipping
builds of the published implementation commit in debug/release; 16 samples per
size and another 16 maximum-size live-stop
samples. Allocation/touch/decoding and harness completion waits are untimed setup.
No Weak retains the allocation. System's actual deallocator observes exactly one
target PCM Arc layout free on worker, zero on control; ticket usage reaches zero
only after that physical free. Main transfer allocator counts are 0/0/0.

| PCM | Profile | Control transfer median / max | Full worker job median / max |
| --- | --- | ---: | ---: |
| 512 bytes | Debug | 2.624 / 3.585µs | 6.850 / 13.090µs |
| 384,000 bytes | Debug | 2.313 / 3.034µs | 58.729 / 677.323µs |
| 64MiB | Debug | 3.325 / 4.627µs | 2671.444 / 4015.004µs |
| 512 bytes | Release | 0.161 / 0.341µs | 1.792 / 4.106µs |
| 384,000 bytes | Release | 0.160 / 0.331µs | 36.115 / 65.158µs |
| 64MiB | Release | 0.491 / 0.851µs | 3874.039 / 4689.370µs |

At 64MiB the actual System::dealloc layout is **67,108,880 bytes**. Release
physical free alone is median 3867.289 / max 4683.551µs; construction ticket peak
is **134,217,728 bytes**. Accepted-original play transfer, including native play
metadata allocation, is median 9.885 / max 10.366µs at this size.

The same maximum-size live PCM at offline stop measures:

| Profile | Offline stop median / max | Manager move median / max | Worker PCM free median / max |
| --- | ---: | ---: | ---: |
| Debug | 8.954 / 10.817µs | 1.453 / 1.762µs | 3745.415 / 5319.491µs |
| Release | 1.983 / 2.434µs | 0.821 / 1.022µs | 4735.130 / 6186.792µs |

Maximum control-side free is **4096 bytes**, with zero target-PCM frees there.
The native Sound remains capable of 64MiB; its multi-millisecond release moved
to worker rather than being disguised as a count quota. Measured control transfer
fits the 0.5ms target in this workload; host scheduling/WCET and real native
pause/fence times are not guaranteed. The offline stop timing is not a CPAL
hardware timing claim. Final host drain reports generations/outstanding/owners=0.

The full PCM load still runs real Kira for 4096×512 frames through 8ch/48kHz/i16,
with `[32767,-32768,0,0,0,0,0,0]`, including full native unused queue backpressure.
PCM-thread allocations/reallocations/deallocations remain **0/0/0**. Debug p95 /
p99 / max: 451.202 / 1464.482 / 3317.168µs; release: 34.552 / 75.514 / 261.665µs,
against the 10666.667µs block deadline. PCM construction peak=1,537,024 bytes.
Host noise and cross-thread allocator behavior affect these observed tails.

## Actual commands, source configuration and remaining targets

These checks passed, with [build/format evidence](tests/evidence/background-checks.txt):

```sh
cargo test -p reel-media --locked --offline -- --nocapture
cargo test -p reel-media --no-default-features --locked --offline -- --nocapture
cargo clippy -p reel-media --all-targets --locked --offline -- -D warnings
cargo clippy -p reel-media --no-default-features --all-targets --locked --offline -- -D warnings
cargo check -p reel-media --all-targets --target x86_64-pc-windows-gnu --locked --offline
cargo fmt --all -- --check
python3 crates/reel-media/tests/support/verify_kira_fork.py
crates/reel-media/tests/support/run_kira_regression.sh
crates/reel-media/tests/support/run_pcm_probe.sh
cargo tree -p reel-media --no-default-features --locked --offline --edges normal
```

`cargo fmt --all` initially failed with the published crate's missing root
configuration. The exact VCS commit's [rustfmt.toml](https://raw.githubusercontent.com/tesselode/kira/74b3ef0fd8bd15f90e3b469208ee741f04374a62/rustfmt.toml)
contains `hard_tabs = true`; it is restored byte-for-byte, SHA-256
`bce79651b2e2ff42e1c54b9a26c5e17af5222f9f65ab51d9563a48fd159dae84`, with source
record. The actual aggregate command above now passes, covering engine workspace
and its local path dependency. No other original imported source was reformatted:
inverse-patch verification still matches all 126 original files and license/config
hashes; only two production files carry the Kira patch. atomic-arena 0.1.2 is
unchanged registry code. Feature-off normal tree remains free of CPAL/platform,
window/render, decoder and VM dependencies.

Latest [device transcript](tests/evidence/background-device.txt): physical command
`cargo run -p reel-media --example device_probe --locked --offline` exits 2 with
device_unavailable (`/dev/snd` absent). With ALSA_CONFIG_PATH pointing to a virtual
null configuration, the same real CPAL/Kira command exits 0. It reports generated
41146, submitted/presented=0 invalid, latency=0, precision Unavailable; native
pause errno 77 still yields post_stop_fill_calls=0. Explicit host drain reports
zero generations/control owners/outstanding work and two completed teardowns.
This is a null sink, not audible playback or a device-clock measurement.

Remaining: owner review/rerun; actual Windows/N100 8ch mask/order/audio/tails,
Linux/macOS devices, hotplug, hardware suspend/resume/startup faults and any
future confirmed native clock. Windows is cross-checked only; unknown multichannel
device layouts are negotiated to known layouts or Unsupported. No main change,
merge, Actions, creator-guide edit, video/DSL/full-player expansion is included.
The owner maintains architecture/guides; implementation/docs/evidence stay within
media and the approved Kira fork. PR #14 remains draft.
