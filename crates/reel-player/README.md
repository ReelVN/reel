# Reel window / renderer diagnostic

This is a thin native assembly using the production `RenderFrame` → wgpu path.
It is **not an Aside story session** and does not execute the VM.

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
adaptation and actual source execution remain separate acceptance work.
