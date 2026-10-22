# Reel player assembly

`reel-player` joins the Aside session driver, common UI, resource services and
renderer. `PlatformHost`, `Presentation` and `StoryRuntime` use portable types.
The `winit` feature selects native event conversion; `window` combines the built-in
Winit and WGPU adapters. Architecture and author workflows are documented in the
[engine contract](../../docs/engine-architecture.md) and
[creator guide](../../docs/guide/en.md).

## Session driving

[`AsideSessionUi`](src/aside_session.rs) consumes a compiled story, completes its
module initializer and starts the selected entry. [`StoryRuntime`](src/story_runtime.rs)
drives bounded VM, GC and retirement work from a monotonic host clock.
`BudgetYield`, waiting, resource blocking, faults and completion retain distinct
states. Closing starts cancellation; keep driving cleanup until obligations end.
Fault deliveries remain rooted until transferred or discarded.

[`Assets`](src/assets.rs) prepares fonts and images through `AssetIo` and
`WorkExecutor`. Directory and packaged sources share content-versioned resource
leases. A candidate scene waits for its eligible resources while the previous
complete view remains presented. Asset errors preserve resource paths and source
locations. Host options configure real input/resident/temporary byte budgets.

[`media`](src/media.rs), [`saves`](src/saves.rs) and
[`localization`](src/localization.rs) adapt their services to the same session.
Source compilation installs generated catalogs; release packages resolve the
existing text/resource archives.

## Presentation and input

Call `driver.sync` with the mounted SessionWidget, then `driver.record` to obtain
the actual `RecordedSource` and its projection receipt. Keep that receipt with the
candidate through compilation and presentation. Only acceptance of that exact
candidate calls `driver.presented`. Pending model changes retain their separate
projection; completion polling releases resources independently.

[`SessionUi`](src/session.rs) owns the author content LayerStack: base StageWidget
plus Popup layers. Host background and preview chrome remain outside that root.
Whole-stage transitions capture author content together; named transitions use
one physical subtree. Popup ownership/retirement remains in the logical Stage.
Common logical-canvas scaling, layout and accepted geometry drive both drawing
and input. Popup ancestors supply anchor/lifetime/visible/enabled eligibility;
ancestor clip and compositing effects stop at the layer boundary.

Down/Up actions retain their accepted projection. Shared input covers mouse,
touch, keyboard and gamepad navigation, including focus, modal scopes and capture
cancellation. After resolving/clearing the bridge, call `presented` or
`clear_presented` to deliver cancellation and wake newly runnable work.
Accepted text edits keep their ordered replay through delayed undo/redo and focus
changes. Native IME/clipboard adapters feed the same editor transaction path.

## Native host lifecycle

Construct `Presentation` with matching bridge/backend SurfaceInfo. On suspension,
cancel active input, suspend presentation/backend and pass monotonic time to
StoryRuntime. Resume under a newer surface epoch before resuming activity time.
Device recreation also replaces renderer generation. VM state and prepared CPU
sources survive these surface changes. Zero-sized surfaces skip acquisition.

Expose redraw uses the last accepted frozen CPU frame. Temporary residency
pressure retries after completion; permanent refusal waits for model/size change.
Window close drives task/root cleanup even while suspended. A completed story
can keep its final picture and mounted event handlers until explicitly closed.

## Native diagnostics

Set `REEL_DIAGNOSTIC_FONT` to a TTF/OTF path before the diagnostic launch:

```sh
cargo run --locked -p reel-player --features window --bin reel-diagnostic
cargo run --locked -p reel-player --features window --bin reel-session-host
```

The session host uses `REEL_SESSION_FONT`, falling back to the diagnostic font.
F5 exercises device recreation and F6 surface suspension in diagnostic windows.
`REEL_DIAGNOSTIC_TRACE=1` logs frame/epoch and pointer routing. These commands run
against the actual native display and adapter.

```sh
cargo test --locked -p reel-player --lib
cargo check --locked -p reel-player --no-default-features --features winit
cargo test --locked -p reel-player --features window actual_window_retry_and_recovery_contract -- --ignored --nocapture
cargo run --locked -p reel-dev -- deps
```

Native presentation, IME/clipboard and physical gamepad input require checks on
the target device. System accessibility adapter integration remains open.
Headless source/ASB and accepted-frame routing checks use the same library paths. See [renderer ownership](../reel-render/README.md) and
[author input](../../examples/author_input/src/main.aside).
