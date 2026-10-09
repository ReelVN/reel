# Reel session host (S01)

This crate owns the bounded Rust session model specified by architecture 1.1.
It does not execute Aside, implement a VM adapter or provide snapshot/restore.
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
and independent acceptance. Real VM waits, deliveries, native registration,
BudgetYield/ResourceBlocked and restore remain unconnected and unverified.
No test in this increment executes or simulates Aside bytecode.

Local input dispatch uses Session::handle_local_input(owner, action), which
allocates the next sequence from the Session's current high-water mark at dispatch
time. SessionUi owns no independent counter, so an already-used Session or later
external input through session_mut cannot strand local clicks behind an old
counter. handle_input remains the explicit-sequence replay/external path; local
sequence exhaustion returns SequenceExhausted. A zero-line dialogue is a valid
interaction and completes with one accepted Advance, retaining exactly-once
history/completion semantics.
