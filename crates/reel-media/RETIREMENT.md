# reel-audio retirement contract (AudioOutput 1.1)

The mixer is maintained in [reel-audio](../reel-audio/README.md), with original
licenses and [source provenance](../reel-audio/PROVENANCE.md). Resource ownership
and main-track control are ordinary workspace code. Unused mixer resource kinds
have zero capacities in the device bridge.

## Legal interleaving and bounded transfer

With sound capacity 1, A can free its arena reservation before publishing to the
unused queue. Control can legally play B in that interval. A then publishes; when
B finishes, the unused queue is full. The guard must run **before
DrainFilter.next()** can remove B. B stays in its arena until there is room;
retirement never drops PCM or waits for control on the realtime thread.

The test-only scheduling hook uses real StaticSound/Renderer PCM and pauses that
legal interval. It checks quota 0/1, the capacity clamp for `usize::MAX`, and that
play(C) performs no implicit old-Sound reclamation. The hook and module declarations
are `cfg(test)` only. Current commands from the repository root:

```sh
cargo run --locked -p reel-dev -- audio-regression
cargo run --locked -p reel-dev -- audio-regression --release
cargo test -p reel-media --no-default-features --locked -- --nocapture
```

The Rust runner executes `cargo test -p reel-audio --lib --no-default-features`
against the root workspace lock. Raw results stay under `target/reel-artifacts/`.

Reel reserves a real destination queue chunk before taking unused Sound. Explicit
`reclaim_unused(limit)` transfers at most min(limit,N); it runs no physical PCM
free. A full/contended destination leaves Sound in reel-audio. Quota exhaustion,
including zero quota, conservatively requests another maintenance turn. A host
must grant a positive quota to progress rather than spin within one tick.

Stopped Sound remains charged before actual retirement. Same-mixer A→B under
one-ticket or one-PCM byte capacity returns Busy until the worker physically
releases A. BudgetedSound drops native Sound/PCM before its ticket. Accepted
DecodedSound ownership also moves to the worker; accepted aliases cannot leave a
last PCM destructor on a preempted play caller. Rejection returns the unchanged
unaccepted request. See [RECLAIM-DESIGN.md](RECLAIM-DESIGN.md) for queue/credit bounds.

## Publication and wakes

Real Sound::finished() publishes observation; notification follows the complete
Renderer retirement publication phase. An early empty control drain cannot clear
an unpublished retirement observation. The notifier touches coalesced atomics;
generic WakeSignal notification runs on its control thread, not PCM. Its 2ms
polling interval is not a scheduler latency guarantee.

The host acknowledges its wake before draining sources. Quota/full requests and
worker completion schedule another turn even while UI sleeps or no further PCM
callback occurs. Empty observation never proves all active/in-flight PCM residency
ended. Mixer tests retain these scheduling assertions and final host-drain checks.

## Historical correctness and cost evidence

The [fixed direct-control record at 97ecb904](https://github.com/jhq223/reel/blob/97ecb904ca0ab8cb8aebb1550e87b765e4f894dc/crates/reel-media/RETIREMENT.md)
includes the upstream capacity-1 panic and the original guard/quota regressions.
Its 64MiB release median 2160.502µs and maximum 3025.448µs **failed the 0.5ms
control-reclaim target**. A count quota cannot split an indivisible destructor.
Those historical API/timing statements are not current verification claims.

The current service moves physical destruction to its non-RT worker; native
pause/fencing and final host join remain separate boundaries. Measure the current
checkout with `audio-probe`; retain raw results under ignored artifacts rather
than updating source documentation with a generated transcript.
