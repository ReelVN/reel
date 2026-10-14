# Reel session host (S01)

This crate owns the Rust session model specified by architecture 1.1.
The Rust model remains independently usable. The `aside` adapter executes real
source programs, owns story waits and publishes the common author stage.
Snapshot/restore is not implemented.
PR #50 / Issue #48 introduces it as one substantive engine crate; UI remains
in reel-ui and platform/window assembly remains in reel-player.

Session::new receives a caller-assigned unique SessionId/Epoch.
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

## Ownership and retirement

Owners, dialogue lines, choices, text and complete history grow with author
content. Storage is reserved before publication; allocation or validation failure
leaves identities and history unchanged. History is retained without eviction.
StorageUsage reports backing capacity, not allocator overhead. Completed Retired
owner slots are reused; live and revoked slots retain their ownership obligations.

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
BudgetYield/ResourceBlocked. Native Windows execution checks cover this adapter; restore remains outstanding.
The S01 Rust-model tests do not execute or simulate Aside bytecode.
PR #55 compiler/VM regressions execute real bytecode and host transitions.

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
`source::compile_module` is shared with the author CLI; `compile_story` selects
an executable entry. The prelude includes `narrator`, `choose`, `text`, `row`,
`column`, `stack` and the `stage` namespace. Selection maps the exact module-level HIR function symbol
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
Reader references and retained keys each use one rooted VM array, so option count
does not consume one host root per row or key.

Text reads use chunks bounded by min(4096, heap mutation limit), with UTF-8
decoding after the complete copy. This is a per-read chunk size, not a text length
limit. VM values use the configured heap budget; Session copies reserve their
actual storage. Choice lookup work is metered separately; string copying and
publication are synchronous, not
charged as VM instructions or wholly covered by player `host_work`.

Revocation first establishes all peer retirement obligations, then releases
single roots against a frozen high-water end (including holes), and drives task
cleanup independently. This adapter owns no non-task RootRange or external
resource gate. GC uses the VM provider, not a heap-only substitute. Result delivery
transfers its root before retirement; the retained result slot is reused by a
later result, so an earlier bare Value is not a permanent host root.

Native Windows checks execute compiler/VM, wait/root, rejection, GC, cancellation
and player terminal contracts. Character objects, multiline say, observable bindings, media waits and save/restore remain outside this adapter. Node-owned button callback tasks now share the VM with the main story.

### Source linkage and runnable contract

The host seed carries the requested content_version; native ABI remains v1.
Lowering coalesces symbol/node matches only when their independently checked
registrations identify the same NativeId. Conflicting IDs still reject.

```sh
cargo test --locked -p aside-vm -p aside-compiler -p reel-engine -p reel-player
cargo run --locked -p reel-engine --example story_contract
```

The [story](examples/story_contract.aside) and [headless host](examples/story_contract.rs)
compile and execute narrator/choose, advance real waits and retire their owner.
These are CPU/library checks. The author CLI separately provides the window/ASB
entry; its native device behavior has its own verification boundary.

## Author stage

`reel-ui::stage` is the shared node model. Nodes returned by `text`, `row`,
`column` and `stack` have real Host identity and an ordinary shared property
table. Holding any node keeps its complete unmounted component alive through
private parent and child references. A stage root keeps its subtree alive across VM collection. `stage.replace`
retires the old tree; keeping an old script handle does not keep retired content
alive. Session cancellation retires the stage along with its task.

`SessionVm::sync_stage` publishes changed property tables; unchanged tables do
not recopy text or bump the stage revision. Author properties include x/y,
optional width/height, opacity, visible, clip, gap, padding, font_size, color and
background; a text node also exposes value. Colors use #rrggbb or #rrggbbaa.
Unknown fields and retired handles fail at script access. Property writes use
the Host's pure validator before committing the value, so invalid types, colors
and numeric ranges are catchable at the assignment and preserve the old value.
Using a retired node in a native call also produces a catchable `InvalidState`
at that call; invalid heap identities remain host errors.

The [author example](../../examples/author_stage.aside) uses functions, children,
conditions and loops, then changes existing nodes between two dialogue waits.
Run it with `cargo run --locked -p reel-cli --features window -- dev examples/author_stage.aside --assets target/reel-artifacts/author-assets`.

`button` adds an optional label and `on_click` closure to the same node model;
its children use stack layout. All nodes expose `enabled`, including containers.
`SessionVm::click` starts a node-owned task only for a mounted, visible, enabled
button. The player invokes it after accepted input routing. The callback receives
`{target: node}` and defaults `run` ownership to that event node.

`SessionVm::step` returns `SessionStep::Vm` for real VM outcomes, `Progress` for
handled callback completion/cleanup, and `Idle` when no task can run. Synchronous
segments retain their task across budget slices and native continuations; waits
and terminal results permit another task. Multiple consumers can await one
activity. Callback retirement releases actual VM storage before reusing its
session owner slot. `discard_delivery` therefore requires a mutable session.
`narrator` and `choose` remain main-story operations; callback use raises a
catchable context error. See the [button example](../../examples/author_buttons.aside).
