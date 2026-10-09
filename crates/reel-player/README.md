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
