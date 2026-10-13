# Reel player assembly

This is a thin native assembly using the production `RenderFrame` → wgpu path.
The diagnostic binaries below use Rust requests and do not execute the VM.
`shell::run_story` connects the Aside session driver to this same native window.

```sh
# Optional: a real TTF/OTF font for Masonry/Parley text, registered before recording.
export REEL_DIAGNOSTIC_FONT=/path/to/font.ttf
cargo run --locked -p reel-player --features window --bin reel-diagnostic
```

The font source reserves space in the shared CPU resource budget before reading; image
and font ContentVersion values are BLAKE3 hashes computed during preparation.

Drag the shapes/white indicator; scroll to change the root transform. The checker
image, overlapping transparent group, clip and optional Parley label use the same
render path as readback tests. F5 destroys the actual device and exercises device
recreation; F6 exercises the same surface suspend/resume handlers as the lifecycle
callbacks; Escape closes cleanly. Set `REEL_DIAGNOSTIC_TRACE=1` to log accepted
frame/epoch identities alongside routed physical pointer coordinates.

`window.rs` owns the native window, surface, configuration, prepared resources and
frame assembly. `shell.rs` maps winit events and wake-ups. `diagnostic.rs` is an
ordinary Masonry widget. PresentationBridge routes all input against the last
accepted geometry. Compilation/upload never installs geometry. The shell submits,
calls surface present, and only then resolves a notice for that exact frame.
Completion polling is independent. Zero size/suspend performs no acquisition;
resize/DPI/resume/device recovery replaces the epoch. Device recovery increments
generation, preserving the widget and prepared CPU sources. Suspend drops surface
before window and drains queued work; shutdown does the same before exit.

Frame and geometry storage grow with the recorded scene within their byte budgets;
the window does not impose diagnostic node, command, mask or nesting counts.
Compilation honors the next step's reported minimum work and allocation budget.
Prepared resource slots match the actual startup resources, and story windows do
not allocate the renderer diagnostic's checker image. CPU resource residency is
limited by bytes rather than a separate ticket-count quota.

The shell accepts opaque sRGB surfaces with FIFO presentation. Other surface
profiles, IME/accessibility platform integration and touch/pen input remain future
adapter work. Unsupported drawing records and quota refusals are logged with their
record location where applicable and retain the last accepted view.

See [device contract](../reel-render/R02-INTERFACE.md) and
[verification and remaining scope](../reel-render/R02-REPORT.md).

## Rust session-host window (S01)

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
Input in this version uses mouse clicks and scrolling.

Author software-GPU evidence uses Xvfb/Vulkan llvmpipe, not physical hardware.
Independent checks and remaining limitations are tracked in PR #50. VM wait/root
adaptation and native Windows source execution are covered by PR #55 library tests.

SessionUi dispatches through Session::handle_local_input rather than resetting
a private input counter. Existing-session handoff and subsequent session_mut
input preserve the same high-water sequence. Zero-line dialogue is accepted and
can complete through the normal presented Down/Up path in one advance.

## Aside session library assembly (PR #55)

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
At 110b984, StoryState distinguishes Running, StartingEntry, Finishing,
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
`shell::run_story(story, shell::StoryWindowOptions::new(font_path))`, where `story`
is a prepared `reel_engine::aside::source::CompiledStory`. Options expose the
window title, locale and logical window size (default 960 × 540). That size is
not a fixed author canvas. The font is a TTF/OTF resource used by the existing
text renderer; no system-font or diagnostic-scene fallback is substituted.

Each window turn advances bounded VM, GC and retirement slices within one 2 ms
deadline, then updates the projection once. Synchronous native calls continue
within that deadline instead of waiting for a separate window frame. A published
dialogue or choice finishes the active GC cycle, then sleeps until presented UI
input completes it. VM and GC slices alternate during execution, so discarded
nodes and loop temporaries are reclaimed throughout a long story; instruction
budget yields schedule another turn without forcing a redraw. Surface resize,
suspend/resume and device recovery preserve the same VM. The existing reveal
timer advances at most one grapheme per wake and freezes while a click is held.
Natural completion leaves the window open after owner retirement. Close or
Escape cancels and continues task/root cleanup before exiting, including while
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

The supplied default font covers every supported script, including Chinese. Font
collections share one resource across face indices. To run the existing author
scene contract with real Chinese glyphs on Windows:

```powershell
$env:REEL_SESSION_FONT = "C:/Windows/Fonts/msyh.ttc"
cargo test --locked -p reel-player --lib author_scene_renders_edits_original_nodes_and_keeps_final_picture
```

Author source receives the registered Reel prelude automatically, while module declarations, local bindings and explicit imports take precedence. PNG/JPEG `image` nodes load only when mounted through the background resource loader. Source paths resolve against the source directory in `dev` and the ASB directory in `run`; `--assets <directory>` overrides that root, and library hosts configure `ImageOptions`. ASB build does not copy assets. The previous complete scene remains visible with story input paused until all new images are ready. Loading errors end the run and include the path and constructor location. See the [image example](../../examples/author_images.aside) and [creator guide](../../docs/guide/en.md).

Author nodes share rotation (clockwise degrees), scale_x/scale_y and proportional anchor_x/anchor_y. Negative scale mirrors and zero scale collapses without retiring the node. Position, transform and opacity edits preserve layout and widget identities; the compose pass supplies the same affine to drawing, clipping and presented input geometry. See [author_transforms.aside](../../examples/author_transforms.aside).
