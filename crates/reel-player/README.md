# Reel player assembly

This is a thin native assembly using the production `RenderFrame` → wgpu path.
The diagnostic binaries below use Rust requests and do not execute the VM.
The separate Aside adapter described below is library assembly, not a new
verified runnable story binary.

```sh
# Optional: a real TTF/OTF font for Masonry/Parley text, registered before recording.
export REEL_DIAGNOSTIC_FONT=/path/to/font.ttf
cargo run --locked -p reel-player --features window --bin reel-diagnostic
```

The optional font source is capped at 16 MiB and budgeted before reading; image
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

A real font is required (REEL_DIAGNOSTIC_FONT is a fallback); the existing 16 MiB
font-source limit remains. The scene shows two dialogue requests, a choice,
a background branch and completion/cancellation. It does not establish native
story APIs, media, save/load or VM integration.

SessionUi records a projection stamp with each candidate frame. Only acceptance
of that exact PresentationBridge frame enables its input projection. Down/Up
must belong to the same presented projection; a stale screen cannot activate
an unpresented successor. Choice release is consumed, not also a story advance.

Full Parley text layout is kept while cluster/ligature masks reveal its prefix.
Each 35 ms tick reveals at most one grapheme per line; a held gesture freezes
reveal and wake-up does not replay a backlog. This compact 640×420 view supports
at most two dialogue rows or two choice buttons; larger projections explicitly
reject and clear input permission. The model's larger limits do not imply the
view can display them. Input in this version is mouse-only.

Author software-GPU evidence uses Xvfb/Vulkan llvmpipe, not physical hardware.
Independent checks and remaining limitations are tracked in PR #50. VM wait/root
adaptation now exists in PR #55 source; actual source execution remains separate
acceptance work.

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

The existing presented-frame input gate and compact two-row/two-choice,
mouse-only view remain. An oversized projection is rejected before publication.
Cancellation routes through VM owner revocation and bounded root/task cleanup,
never through the Rust model's zero-obligation shortcut. GC is provider-aware.
The embedding application must continue cleanup until its obligations retire.

This is reusable library wiring, not a window/CLI source-file launch path or an
ASB loader. The existing diagnostic/session-host commands remain Rust-only.
At 7d5d987, StoryState distinguishes Running, Faulted(DeliveryId), Finished and
Cancelled. Repeated terminal ticks return the same terminal category without
stepping the VM. fault() offers ephemeral fault inspection while delivery remains
rooted; cancellation invalidates it and starts bounded cleanup. Cancel after
normal completion preserves Finished. A terminal state does not mean cleanup has
finished: continue cleanup_step while cleanup_pending reports work.
The compiled-program constructor is available; decoding/loading an ASB file is
still a separate integration increment.

Native/i686 compile checks and independent source inspection are the available
evidence. VM/Aside execution, player terminal-flow regression execution,
performance, hardware and full story acceptance remain unverified.
