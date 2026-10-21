# Host-owned background PCM destruction (AudioOutput 1.1)

[RECLAIM-DESIGN.md](RECLAIM-DESIGN.md) defines the queue/credit proof and public
API. The host owns AudioReclaimService independently of mixers. Each generation
reserves Renderer/Manager teardown credits before construction; ordinary work
cannot consume them. Old generations remain counted until their jobs/owners end.

Normal control producers reserve a safe rtrb destination chunk before taking
Sound. `reclaim_unused(limit)` reports bounded **transferred_sounds**, not completed
destruction. `reserve_decode_budget` attempts transfer then returns a ticket or
Busy immediately. RT and control never wait for budget completion. The pool stays
charged until physical release; worker completion wakes the sleeping host to retry.

Successful play also transfers the original owned DecodedSound to an accepted-data
job. Rejected requests retain caller ownership. Fill/Mixer Drop only transfer
whole Renderer/Manager shells with reserved credits. An untaken fill inside a
worker-owned Manager can submit its own Renderer credit on that worker. Neither
control shell joins or destroys large PCM. Final host exit drops control owners
then explicitly calls `drain_and_join`; outstanding owners return Busy. Service
Drop detaches a draining worker. Worker job/wake failures remain observable and
refuse new acceptance while existing work continues draining.

## Bounds and lifecycle

Defaults are G=4 live generations and R=64 normal queued jobs. The host configures
nonzero capacities; there are no additional fixed generation or job-count maxima.
There are R+1 queued/in-flight normal jobs and 2G total teardown credits. Sound
capacity N has N active/pending reservations plus N unused slots.
Actual PCM residency remains charged to the caller's BudgetPool throughout
retirement; queue counts do not estimate its byte size. Queue metadata is
preallocated. Accepted aliases do not duplicate PCM.

Sounds have no separate frame-count or duration ceiling. A sound's ticket must
cover input vector capacity plus converted Frame storage. Stop closes/fences
callback entry and disposes the
fill shell on its caller. Stopped means no more supply; asynchronous residency
still holds tickets. Native pause, stream destruction and in-flight fencing have
no 0.5ms promise. Final explicit host join is a shutdown wait, not a frame task.

## Reproducible validation

```sh
cargo test -p reel-media --locked -- --nocapture
cargo test -p reel-media --no-default-features --locked -- --nocapture
cargo run --locked -p reel-dev -- audio-regression
cargo run --locked -p reel-dev -- audio-regression --release
cargo run --locked -p reel-dev -- audio-probe
```

Media regressions retain same-mixer single-ticket/single-PCM A→B, quota 0/1,
ordinary saturation, reserved teardown credits, nested untaken Renderer, old
live-generation backpressure, constructor/fill failures and sleeping/no-callback
maintenance. The real-reel-audio probe builds shipping debug/release libraries and
observes allocator calls only on the measured PCM thread through 8ch conversion.

The reclaim probe separates control ownership transfer, full worker job and actual
System::dealloc. It checks the worker thread and verifies the ticket/byte charge
immediately before **and after** System::dealloc returns; only completed jobs may
leave usage zero. It also stops a live 64MiB Sound, checks small caller frees,
post-stop silence and final generations/outstanding/owners/errors=0. These are
container observations, not scheduler/WCET or native-device guarantees. All raw
stdout/stderr, command exits and generated copies go to `target/reel-artifacts/`.

The [fixed background implementation record at 1f05685a](https://github.com/jhq223/reel/blob/1f05685a0267a4bee7ed4394d696c2f68c161cf6/crates/reel-media/BACKGROUND-RECLAIM.md)
retains earlier measurements and transcripts. It establishes neither current
checkout timings nor physical hardware repair. The [direct-control historical
record](RETIREMENT.md) explicitly preserves its failed 0.5ms target.
