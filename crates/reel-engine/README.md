# Reel session host

`reel-engine` owns sessions, reading contexts, interactions, history, author
activities and the Aside host adapter. UI types come from `reel-ui`; native
assembly lives in `reel-player`. The
[architecture](../../docs/engine-architecture.md) defines the v1 ownership model.

## Interactions and retirement

A `Session` receives a unique caller-assigned SessionId/Epoch. Dialogue admission
reserves a complete group and history before publication. Advance first completes
reveal, then completes the wait. Choices retain immutable business keys qualified
by interaction identity. Input sequence, session/epoch, interaction and owner
checks reject stale delivery. Local input allocates its sequence from the Session;
external/replay input supplies an explicit sequence.

One completion slot retains its terminal result until claimed. Cancellation
permits cleanup; actual delivery rechecks live task/wait/owner identity. Text,
history and owner storage reserve capacity before publication. History remains
owned by its reading context until cleared or that context exits.

Owner revocation closes new admission, freezes root-retirement requirements and
coordinates task, VM-root and external-resource cleanup. Charge actual cursor
progress, including empty slots. Owner reuse waits for every recorded obligation;
unreachable objects are reclaimed by the VM's separate GC.

## Aside integration

[`source`](src/aside/source.rs) supplies the shared compiler/native/type schema.
`SessionVm::load_story` performs trusted runtime admission and installs generated
translation catalogs. Module initialization finishes before the selected ordinary
entry. Source and ASB consumers share this path and preserve source identities.

[`SessionVm`](src/aside/mod.rs) tracks the main task and node-owned callbacks.
Publication prepares values and session state before commit. Wait delivery
retains original immutable result identities. `step` distinguishes real VM
outcomes, handled progress and idle state; cleanup remains independently driven.

[`scope`](src/aside/scopes.rs) owns mounted content and callback work; `finish`
returns through the same Task's host boundary. [`reading`](src/aside/narrative/contexts.rs)
adds dialogue, choice, history and temporary controls to that lifetime. Nested
reading moves and restores parent state with its original model identities.
Captured Reading/Dialogue/Choice objects retain their context; getters select the
active context. Preferences and read progress retain their shared ownership.

[`stage`](src/aside/stage.rs) projects Aside nodes into the common Stage.
Unmounted components retain logical parent/child ownership. Retirement cancels
owned tasks and invalidates mutable operations. Unchanged property projections
reuse text/resources and preserve widget identity. The shared schema supplies
validation, source types, defaults and editor documentation.

## Checks and examples

```sh
cargo test --locked -p reel-engine --lib
cargo run --locked -p reel-engine --example story_contract
```

The [headless story host](examples/story_contract.rs) drives real waits and owner
retirement. [Author examples](../../examples) compose those same interfaces.
[Source adapter tests](src/aside/tests) cover source/ASB execution, cancellation,
reactivity, snapshots and resource ownership. The
[creator guide](../../docs/guide/en.md) explains author workflows.
