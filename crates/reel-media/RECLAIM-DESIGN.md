# Host-owned background destruction contract

Contract 1.1 covers the reel-media device lifecycle and the reel-audio mixer.
PCM stays budgeted through physical release, callbacks use the native gate/fence,
and the CPAL clock remains Unavailable.

## Interfaces

- `AudioReclaimService::new(ReclaimLimits, WakeSignal)` creates bounded queues and
  one non-RT worker. The host holds it independently of mixers.
- `KiraMixer::new(format, MixerLimits, &service)` reserves one generation and two
  unique teardown credits before constructing reel-audio. Exhaustion returns Busy.
- `reclaim_unused(limit)` reports `transferred_sounds`, not completed destruction.
  It obtains an rtrb write chunk before reel-audio `try_take_unused_sound()` purely moves
  a Box. Full queue/producer contention leaves Sound in reel-audio's unused queue.
- `reserve_decode_budget` attempts transfer then normal reservation. Busy remains
  valid until the worker actually drops all internal PCM references and tickets.
- `play` reserves a normal write chunk before accepting. Success transfers the
  original owned DecodedSound as well as the Sound held by reel-audio; failure returns
  the unchanged request. DecodedSound is not Clone and exposes no PCM Arc/Weak.
- `service.status()` reports occupancy, completion/error and background cost.
  `drain_and_join()` is explicitly blocking and rejects while control-side owner shells
  remain. A Renderer still inside a queued Manager is background owned even
  before its separate credit is submitted. Only final host exit calls it. Service Drop
  requests detached draining; Mixer Drop never joins or destroys the service.

## Credit and ownership proof

The normal rtrb queue has R slots and one worker in-flight object. Producers are
serialized only on the control side; ordinary calls use try_lock. A successful
`write_chunk_uninit(1)` is actual queue credit. Only after obtaining it may the
control side pop a retired Sound or accept play. `fill_from_iter(once(job))`
initializes/commits that reserved slot without a send-failure branch. No pop then
try_send/drop fallback exists. The worker never holds this producer mutex during
destruction; PCM never accesses it. Full/contended transfer makes no progress.

At most G generation permits exist, including old generations kept alive by any
accepted-data, retired-Sound, Renderer or Manager job. Each reserves exactly two
teardown credits before reel-audio construction. The separate teardown queue has 2G
slots. Every queued/in-flight or not-yet-submitted teardown object owns one unique
credit, so a credit outside the queue proves a free slot when its producer is
locked. Ordinary work cannot consume it. The credit stays attached to the job
through destruction; a generation is released only after all its internal jobs
and owners have finished. Queues use existing rtrb 0.4.0 safe chunk APIs.

The fill captures `Option<Renderer>` inside a deferred Drop shell. Drop reserves
its guaranteed teardown chunk before taking that Option, then commits the whole
Renderer. Mixer owns `Option<AudioManager>` in an equivalent shell. Manager may
still contain an untaken fill: its background destructor submits that Renderer
with its separately reserved credit. Construction abort after reel-audio creation uses
the same shells. Validation/service-start failures occur before owning PCM.

Sound:Send allows moving the complete Box<dyn Sound>, including BudgetedSound's
ticket. Job payload fields drop before credits; native Sound/PCM fields drop
before their BudgetTicket. Original DecodedSound references on successful play
also belong to background jobs, avoiding the last-reference race after control
preemption. Refused/unaccepted requests remain the caller's ownership. Worker
completion wakes the same host scheduler only after actual destruction; stopping
alone never returns residency budget.

Queue/in-flight bounds are R+1 ordinary jobs, 2G total reserved teardown credits,
and G live/old generations. Each generation has at most 2N reel-audio-resident sounds
(N active/pending and N unused). PCM residency is charged to the caller's actual
BudgetPool; queue lengths do not imply a maximum sound size or duration.
Accepted-data aliases do not duplicate PCM. Queue metadata is fixed at
construction. Each sound's ticket covers input vector capacity plus converted
Frame storage and retains that real construction charge until physical residency
ends. There is no additional per-sound frame limit or estimated PCM ceiling.

The worker catches job/wake panics, records a fault, wakes control when possible
and continues draining the already-owned queues. New acceptance/generations then
refuse; reserved teardown remains usable. The consumer stays alive until closing,
all generations gone and outstanding jobs drained. No valid worker-error path
disconnects an accepted object's destination or falls back to control destruction.

AudioOutput.stop still synchronously closes entry, fences an in-flight call and
destroys the fill shell. Stopped means supply stopped, while tickets describe
asynchronous residency. Native pause/fence time has no 0.5ms promise. The measured
goal applies to control transfer work; background PCM destruction is separately
measured, including the existing 64MiB measurement workload.
