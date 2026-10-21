# Reel player assembly

This is a thin native assembly using the production `RenderFrame` → wgpu path.
The diagnostic binaries below use Rust requests and do not execute the VM.
`shell::run_story` connects the Aside session driver to this same native window.

```sh
# Optional: a real TTF/OTF font for Masonry/Parley text, registered before recording.
export REEL_DIAGNOSTIC_FONT=/path/to/font.ttf
cargo run --locked -p reel-player --features window --bin reel-diagnostic
```

Author images and fonts use `assets::Assets`, with `DirectoryIo` for background
reads and `ImageExecutor` for PNG/JPEG probing and decoding. `AssetOptions` controls
the root and input/resident/temporary byte budgets. Image and font ContentVersion
values are BLAKE3 hashes computed once from the encoded input during reading.
`Assets::with_adapters(options, |wake| (io, executor))` lets a host supply the
existing `AssetIo` and `WorkExecutor` contracts; pass it to `with_assets` and install
`set_resource_waker` before driving the session. Closing the session closes its
resource service; existing frame leases remain valid until released.

Image work kinds are version 1 `image.probe` and `image.rgba8`, both Decode.
They borrow the same `Arc<OwnedBytes>` compressed input. The probe returns 24
little-endian bytes: width and height (u32), RGBA length and decoder output length
(u64). The RGBA operation returns straight-alpha sRGB pixels. Reservations use
CpuInput, CpuOutput, Temporary and at least 512 Completion bytes. The directory
adapter accepts full and range reads; persistence durability belongs to the
separate storage service.

Drag the shapes/white indicator; scroll to change the root transform. The checker
image, overlapping transparent group, clip and optional Parley label use the same
render path as readback tests. F5 destroys the actual device and exercises device
recreation; F6 exercises the same surface suspend/resume handlers as the lifecycle
callbacks; closing the window completes cleanup. Set `REEL_DIAGNOSTIC_TRACE=1` to log accepted
frame/epoch identities alongside routed physical pointer coordinates.

`window.rs` assembles the window, backend, prepared resources and frames.
`platform::winit::WinitHost` implements platform commands and `Clock`; `shell.rs`
maps native events. `gpu::WgpuBackend` owns the surface, configuration and GPU
submission. `presentation::Presentation` coordinates redraw and accepted frames.
`diagnostic.rs` is an ordinary UI widget. PresentationBridge routes input against
the last accepted geometry. Compilation/upload never installs geometry. Only
successful backend presentation resolves acceptance for that exact frame.
Completion polling is independent. Zero size/suspend performs no acquisition;
resize/DPI/resume/device recovery replaces the epoch. Device recovery increments
generation, preserving the widget and prepared CPU sources. Suspend drops surface
before window and drains queued work; shutdown does the same before exit.

The public `PlatformHost`, `Presentation` and `StoryRuntime` have no native types
or required native dependencies. Enable `reel-player/winit` to use only the Winit
host, or `reel-render/wgpu` to use only the WGPU adapter. The `window` feature
selects the built-in combination. A custom host implements redraw, wake deadline,
before-present and exit commands; it converts input into the existing UI events.
Construct `Presentation` with a fresh bridge and matching `SurfaceInfo`, configure
the backend, then call `render` with a frame-building closure. That closure calls `driver.record(&mut bridge, root, frame_id)` once to obtain both the
`RecordedSource` for frame building and its `RecordedProjection` receipt. Pass
the SessionWidget ID returned at mount, the same `root` used by `driver.sync`.
Recording also works before the first sync or after sync returns while waiting
for stage preparation; it records the tree that is actually mounted. The receipt freezes the actual SessionWidget watermark and StageWidget identity;
pending model edits cannot stamp the previously published widget tree. Only an
accepted result calls `driver.presented(&bridge, &receipt)`. The receipt must stay
with that candidate through compilation and presentation; an old source cannot
be used to mint a new receipt. `poll` uses a reusable completion buffer and never
accepts input.

On suspend, cancel active input, suspend presentation/backend and call
`StoryRuntime::suspend` with the host's monotonic nanoseconds. On resume, replace
the surface with a strictly newer epoch, configure its backend, then call
`StoryRuntime::resume`. Activity time excludes the suspended interval, while VM
deadlines keep using the real host clock. Closing still drains task retirement
while suspended. The Winit adapter connects native IME enable/disable, preedit,
commit and candidate-area placement to the shared editor. Desktop clipboard
copy/paste uses arboard on Windows, Linux and macOS. Candidate-window placement,
focus transitions and clipboard behavior still need target-device acceptance;
system accessibility adapters remain open.

```sh
cargo test -p reel-player --no-default-features portable_host_runs_story
cargo check -p reel-player --no-default-features --features winit
cargo run --locked -p reel-dev -- deps
# Requires a native Windows or Linux X11 display and an actual graphics adapter:
cargo test -p reel-player --features window actual_window_retry_and_recovery_contract -- --ignored --nocapture
```

Frame and geometry storage grow with the recorded scene within their byte budgets;
the window does not impose diagnostic node, command, mask or nesting counts.
Compilation honors the next step's reported minimum work and allocation budget.
Prepared resource slots match the actual startup resources, and story windows do
not allocate the renderer diagnostic's checker image. CPU resource residency is
limited by bytes rather than a separate ticket-count quota.

The shell accepts opaque sRGB surfaces with FIFO presentation. Other surface
profiles, system accessibility integration and pen-specific input remain future
adapter work. Unsupported drawing records and quota refusals are logged with their
record location where applicable and retain the last accepted view.

See [device contract](../reel-render/R02-INTERFACE.md) and
[verification and remaining scope](https://github.com/jhq223/reel/blob/355571b18bd4024914438a7ac6bd94ebf3c7826f/crates/reel-render/R02-REPORT.md).

## Rust session-host window

The additional binary exercises reel-engine with real Label/Button controls and
the existing window/RenderFrame/wgpu path. It sends explicit Rust host requests,
not Aside code. The original reel-diagnostic entry remains available.

```sh
export REEL_SESSION_FONT=/path/to/font.ttf
cargo run --locked -p reel-player --features window --bin reel-session-host
```

A real font is required (REEL_DIAGNOSTIC_FONT is a fallback). The scene shows two dialogue requests, a choice,
a background branch and completion/cancellation. It does not establish native
story APIs, media, save/load or VM integration.

SessionUi records a projection stamp with each candidate frame. Only acceptance
of that exact PresentationBridge frame enables its input projection. Down/Up
must belong to the same presented projection; a stale screen cannot activate
an unpresented successor. Choice release is consumed, not also a story advance.

Full Parley text layout is kept while cluster/ligature masks reveal its prefix.
Each 35 ms tick reveals at most one grapheme per line; a held gesture freezes
reveal and wake-up does not replay a backlog. Labels and buttons use their natural
content height; mouse-wheel scrolling reaches every row within the viewport.
Dialogue rows, choices and their text have no additional fixed count or length
ceiling. All choices share one projection owner, while ChoiceId
preserves each business key. Scrolling invalidates the old input stamp and any
held gesture; a new accepted frame must establish the new hit geometry.
Input supports mouse clicks, scrolling, keyboard activation and independent touch contacts. Gamepad navigation uses the D-pad/left stick, south-button confirmation and east-button Back. Escape also routes Back through the focused UI. The independent `gamepad` feature supplies `platform::gamepad::GamepadSource` on Windows/Linux/macOS through pinned Gilrs; other platforms can inject `reel_ui::input::InputEvent` directly. `NavigationInput` exposes its next repeat deadline so idle hosts can sleep. Backend queues are bounded, device disconnect cancels held contributions, and focus loss/suspension retires the native receiver before rebuilding it on resume. Linux builds of Gilrs require the system libudev development package. Native backend initialization can be checked with `cargo test -p reel-player --features gamepad native_gamepad_backend_initializes_and_drops_without_waiting -- --ignored --nocapture`; this does not verify physical button input. `platform::winit::TouchInput` converts native contacts to portable `PointerEvent` values, retaining identity and pressure; buttons share capture, cancellation and presented geometry across devices. Native text events use `platform::winit_text`; system accessibility adapters
remain future work.

The [original author software-GPU report](https://github.com/jhq223/reel/pull/50)
used Xvfb/Vulkan llvmpipe. It records that environment; current device and platform
acceptance is tracked in [#93](https://github.com/jhq223/reel/issues/93).

SessionUi dispatches through Session::handle_local_input rather than resetting
a private input counter. Existing-session handoff and subsequent session_mut
input preserve the same high-water sequence. Zero-line dialogue is accepted and
can complete through the normal presented Down/Up path in one advance.

## Aside session library assembly

`AsideSessionUi` owns SessionUi and the real SessionVm adapter. `from_source`
compiles through the shared trusted registry and delegates to `from_compiled`.
The latter consumes a CompiledStory without recompiling source; validate checks
entry constraints and its module initializer, then SessionVm::load performs
independent trusted runtime admission. The driver completes the module initializer,
retires that delivery, then starts the selected ordinary entry. A tick executes
at most one VM slice; native publication never recursively runs the continuation.
Waiting, BudgetYield and ResourceBlocked remain distinct. `host_work` meters
choice lookup only; synchronous text copying/publication have independent byte
limits and are not a total dispatch-work meter.

The existing presented-frame input gate applies to content and scroll position.
Admission uses the configured session capacities, without a separate two-row cap.
Cancellation routes through VM owner revocation and bounded root/task cleanup,
never through the Rust model's zero-obligation shortcut. GC is provider-aware.
The embedding application must continue cleanup until its obligations retire.

The existing diagnostic/session-host commands remain Rust-only. Production
`shell::run_story` accepts a compiled story through the same driver and renderer.
StoryState distinguishes Running, StartingEntry, Finishing,
HostFailed(stage), Faulted(DeliveryId), Finished and Cancelled. Repeated terminal ticks return the same terminal category without
stepping the VM or calling the clock. Entry is consumed only after successful
start; a start-capacity error becomes HostFailed(StartEntry), not a script Fault
or an unserviceable Running state. host_failure() retains the original host error
for inspection, including after later cancellation. Drive/finish/cancel errors
have their own stages. Cancel retry preserves recorded retirement obligations;
this is not proof of recovery from arbitrary internal VM corruption.
fault() offers ephemeral fault inspection while delivery remains
rooted; cancellation invalidates it and starts bounded cleanup. Cancel after
normal completion preserves Finished. A terminal state does not mean cleanup has
finished: continue cleanup_step while cleanup_pending reports work.
The compiled-program constructor keeps source compilation and file loading
outside the native event loop.

Native Windows execution checks cover compiled input, initializer/entry ordering,
fault retention across GC, cancellation, normal completion and stable host-capacity
failure. Run `cargo test --locked -p reel-player --lib`. The headless source example
is `cargo run --locked -p reel-engine --example story_contract`; performance,
hardware and the complete author-facing DSL remain separate.

## Aside story window

With the `window` feature, call
`shell::run_story(story, shell::StoryWindowOptions::default())`, where `story`
is a prepared `reel_engine::aside::source::CompiledStory`. Options expose the
window title, locale, optional initial window size and optional author canvas.
With neither size supplied, the native window starts at 1280 × 720; a canvas
configured by the story determines its logical coordinate system. The story configures its font files with
`game.configure(fonts: ["fonts/body.ttf"])` before presentation. Fonts resolve
relative to the asset root and share the existing text renderer and resource leases.

Each window turn advances bounded VM, GC and retirement slices within one 2 ms
deadline, then updates the projection once. Synchronous native calls continue
within that deadline instead of waiting for a separate window frame. A published
dialogue or choice finishes a requested GC cycle, then sleeps until presented UI
input completes it. Allocation pressure, explicit cleanup or resource blocking
starts collection; a low-allocation interaction does not start a scan solely
because its VM slice ended. Active VM and GC work alternate within the deadline; instruction
budget yields schedule another turn without forcing a redraw. Surface resize,
suspend/resume and device recovery preserve the same VM. The existing reveal
timer advances at most one grapheme per wake and freezes while a click is held.
Natural completion leaves the window open after owner retirement.
Closing the window cancels and continues task/root cleanup before exiting, including while
the surface is suspended. VM faults report their source path, line, column and
author-thrown value after
the same cleanup. F5/F6 device diagnostics apply only to diagnostic windows.

Resource-blocked execution gets one incremental collection and retry. If live
capacity remains exhausted, the task stays blocked without polling or losing
its roots; the diagnostic identifies the resource and the window can still be
closed normally. The current shell does not raise VM capacities at runtime.

CPU checks exercise real source execution, bounded scheduling, wait/resume,
choice result delivery and close/normal owner retirement:
`cargo test --locked -p reel-player --features window --lib`.
Native compilation and these checks do not claim a physical GPU test.

Choose font files covering the scripts used by the work, including Chinese. Font
collections share one resource across face indices. To run the existing author
scene contract with real Chinese glyphs on Windows:

```powershell
$env:REEL_SESSION_FONT = "C:/Windows/Fonts/msyh.ttc"
cargo test --locked -p reel-player --lib author_scene_renders_edits_original_nodes_and_keeps_final_picture
```

Author source receives the registered Reel prelude automatically, while module declarations, local bindings and explicit imports take precedence. PNG/JPEG `image` nodes load only when mounted through the background resource loader. Resource paths resolve against the project's `resources/` in `dev` and standalone builds, or the embedded resource manifest in exported ASB packages; `--assets <directory>` overrides that root, and library hosts configure `AssetOptions`. ASB build does not copy assets. The previous complete scene remains visible with story input paused until all new images are ready. Loading errors end the run and include the path and constructor location. See the [image example](../../examples/author_images/src/main.aside) and [creator guide](../../docs/guide/en.md).

Author nodes share rotation (clockwise degrees), scale_x/scale_y and proportional anchor_x/anchor_y. Negative scale mirrors and zero scale collapses without retiring the node. Position, transform and opacity edits preserve layout and widget identities; the compose pass supplies the same affine to drawing, clipping and presented input geometry. See [author_transforms.aside](../../examples/author_transforms/src/main.aside).

## Buttons and portable scheduling

`button(label, on_click: |event| { ... })` uses the shared stage and existing
presented-input routing. `event.target` retains the original node. Common
`enabled` applies to descendants, and callbacks may await activities without
blocking the main story's dialogue. Retiring the node cancels its callback tasks
before their next script statement. Main entry return keeps mounted handlers
usable; idle sessions sleep. See [author_buttons.aside](../../examples/author_buttons/src/main.aside).

`story_runtime::StoryRuntime` is available without the `window` feature. Hosts
provide a monotonic nanosecond clock and `SliceBudget`, call `wake_input` after
input, and use `needs_tick`/`tick_interval` to schedule work. `close` starts
cancellation; continue bounded ticks until `close_complete`. No native window,
event-loop or GPU types enter this scheduler. `AsideSessionUi` and
`PresentationBridge` accept shared input and presentation notices, so another
platform can drive the same script/UI lifecycle.

The built-in `window` feature selects a Winit/WGPU adapter. Only that adapter
depends on `ui-events-winit` for keyboard conversion. VM, stage, input and
scheduler checks, including source/ASB button and animation cases, run without it:

```sh
cargo test --locked -p reel-engine -p reel-player -p reel-cli --no-default-features
cargo run --locked -p reel-dev -- deps
```

These CPU checks prove portable contracts on the host where they run, not
device support on every target. Native IME candidate windows and clipboard
operations need device checks; system accessibility integration remains open.

Author action scopes use `input(bindings:, on_action:)` in Aside or `NodeKind::Input` in Rust, with shared `InputBindings` and `NodeAction` events. See [author_input.aside](../../examples/author_input/src/main.aside). Raw controls route through `TextEvent::Input` before default navigation; its result identifies action consumption within the current modal scope. Input device IDs distinguish keyboard, pointer contact and gamepad. Held state resets on focus/scope loss, presentation revocation and remapping. After `PresentationBridge::resolve` or `clear`, call the fallible `AsideSessionUi::presented` or `clear_presented` to deliver cancellation callbacks, then wake the story runtime if work became runnable.
