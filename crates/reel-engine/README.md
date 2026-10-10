# Reel session host (S01)

This crate owns the bounded Rust session model specified by architecture 1.1.
The Rust model remains independently usable. PR #55 adds an Aside adapter in
`aside`; snapshot/restore is not implemented. Adapter execution acceptance is
pending: source inspection and compilation are not runtime evidence.
PR #50 / Issue #48 introduces it as one substantive engine crate; UI remains
in reel-ui and platform/window assembly remains in reel-player.

Session::new receives a caller-assigned unique SessionId/Epoch and SessionLimits.
The root owner alone submits the main interaction. begin_say atomically copies
a complete plain-text ADV group, locale and content identities, computes grapheme
boundaries and appends history once. reveal_more advances monotonically. First
advance completes reveal; the next ends the wait. begin_choice copies immutable
rows whose ChoiceId combines InteractionId with the business key.

InputSequence, session/epoch, interaction and owner checks reject stale, repeated
or out-of-order input. One pending completion occupies a reserved slot until
take_completion; a successor cannot overwrite it. Cancellation produces
CleanupOnly, never permission to resume. A claimed Resume must still be checked
against live session/owner and the real executor wait at actual delivery.

## Bounded ownership and retirement

Owner/history counts and interaction/history UTF-8 bytes are bounded; admission
failure leaves identities and history unchanged. StorageUsage reports backing
capacity, not allocator overhead. Live→Revoked→Retired records are reserved in
owner slots. Non-retired slots cannot be reused.

revoke_owner_with_retirement freezes the single-root high-water end plus task
and external-resource gates. Expanded root teardown first requires retirement
requirements for all live peer owners; missing requirements reject atomically.
report_owner_root_progress charges actual monotonic cursor progress, including
empty slots. Task and external-resource confirmations clear only their own gate.
Repeated revocation or a zero-obligation helper cannot erase existing obligations.

A real adapter must separately drive single-root release with
slots=min(remaining work,end-cursor), task cancellation and external cleanup.
Host-owned non-task RootRanges need a separate bounded release cursor and keep
the external gate pending. The model does not release VM roots or run GC.
Zero-obligation cancel/end/revoke helpers are for pure Rust callers without VM
obligations. Retirement requires every recorded obligation to finish.

## Verification boundary

Rust tests cover identity, atomic admission, input filtering, history/completion,
retirement and allocation-free accepted hot paths. See PR #50 for actual counts
and independent acceptance. PR #55 connects VM waits, deliveries and native registration in source, preserving
BudgetYield/ResourceBlocked. Runtime acceptance and restore remain outstanding.
The S01 Rust-model tests do not execute or simulate Aside bytecode.
PR #55 adds real compiler/VM regression test sources; these have only been
compiled, not executed.

Local input dispatch uses Session::handle_local_input(owner, action), which
allocates the next sequence from the Session's current high-water mark at dispatch
time. SessionUi owns no independent counter, so an already-used Session or later
external input through session_mut cannot strand local clicks behind an old
counter. handle_input remains the explicit-sequence replay/external path; local
sequence exhaustion returns SequenceExhausted. A zero-line dialogue is a valid
interaction and completes with one accepted Advance, retaining exactly-once
history/completion semantics.

## Aside session adapter (PR #55)

The host-owned ABI v1 registry defines `reel.session` contracts independently
of package claims. The compiler resolver/prelude and runtime admission consume
the same descriptors; runtime checks exact signatures, ABI and capabilities.
`source::compile_story` supports `use reel::prelude::*`, `narrator(text)`
and `choose(options)`. Selection maps the exact module-level HIR function symbol
to its emitted function, including private functions. It rejects required
parameters, captures and nonordinary functions. The emitted module initializer
must complete before the selected entry; default/rest binding remains VM-owned.

`SessionVm` tracks the actual main TaskId. Native publication stages Session
state and retained values, restores history/generation on rejection, and commits
only after Pending acceptance. Resume delivery rechecks exact interaction, owner,
task and wait identities. CleanupOnly never resumes a task. Choice rows read only
own `key`/`label` slots through the public bounded collection cursor; no raw
heap layout or delegate/hook lookup is used. The selected original immutable key
Value stays explicitly rooted through completion instead of being reallocated.

Text reads use chunks bounded by min(4096, heap mutation limit), with UTF-8
decoding after the complete copy. The default 64 KiB interaction limit includes
metadata, locale and generated content identity, so it is not a 64 KiB narrator
payload guarantee. VM constant admission still defaults to 4096 bytes; the
long-key regression source explicitly raises it. Choice lookup work is metered
separately; string copying and publication are synchronous and byte-bounded, not
charged as VM instructions or wholly covered by player `host_work`.

Revocation first establishes all peer retirement obligations, then releases
single roots against a frozen high-water end (including holes), and drives task
cleanup independently. This adapter owns no non-task RootRange or external
resource gate. GC uses the VM provider, not a heap-only substitute. Result delivery
transfers its root before retirement; the retained result slot is reused by a
later result, so an earlier bare Value is not a permanent host root.

Native/i686 all-targets compile checks passed at 6fd0708. Independent source
reviews at 1bfdcd8 found no definite new defect; the later exact-symbol entry
change at 6fd0708 also passed source-only review. Regression sources have not been executed. Character
objects, multiline say, UI callback tasks, observable script UI, media waits and
save/restore are not implemented by this adapter.

### Known compiler-linkage integration defect

A subsequent pure-static build of a story using imported choose/narrator exposed
`LinkContract: ambiguous native binding` in this PR's source linkage. The analyzer
records both the imported symbol and its name-use node; the current adapter emits
both links, while lowering rejects multiple matches for one call. A narrowly
scoped same-symbol/same-native deduplication is implemented and statically reviewed
in the unpublished ASB integration workspace, but is not part of this branch.
Its integration/publication is pending. Native Rust compilation and the earlier
source reviews did not detect this defect; they do not establish that the
story examples compile or run. No VM execution was used to identify it.
