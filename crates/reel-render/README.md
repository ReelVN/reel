# R01 backend contract and semantic consumers

This crate depends on `reel-ui` and `reel-core`; it owns no window or native
handle. `RenderBackend` defines `capabilities`, `configure`, owned `upload`,
borrowed `render_present`, `retire`, bounded `poll` and `suspend`.

`SurfaceInfo` fixes physical size, scale, SurfaceEpoch, color encoding and alpha
mode. `UploadBatch` owns prepared resource leases, pixel/font `OwnedBytes`,
Scope and its vector-capacity ticket. Invalid metadata, overflow, unsupported
payloads or exhausted limits return the original batch before acceptance.
Accepted batches reserve resource, byte, metadata and terminal-event capacity.
`max_upload_bytes` bounds the sum of payload-capacity tickets across **all pending
batches**; `max_retained_bytes` independently bounds pending plus resident storage.
Every terminal upload outcome releases queue byte capacity; completed residency
remains charged. Rejection returns the original batch and its tickets unchanged.
`UploadId` has at most one completed/failed/cancelled terminal outcome.

`RecordingBackend` retains CPU upload storage as simulated residency. Explicit
`complete_upload` creates ready resources; `render_present(&RenderFrame)`
retains an owned frame clone until separate `complete_render(SubmissionId)`.
`PresentAccepted(FrameId)` is a **simulated contract result**, never a device
claim. Retire prohibits new source leases; existing upload/build/frame leases
can still submit. Retired residency is reclaimed only after the final source
lease and simulated rendering completion. Coordinate registry retirement too.
Each retired event retains the precharged version metadata until event drop.

`NullBackend` uses the same upload ownership contract and always reports
`Skipped(NullConsumer)` for rendering. Neither consumer rasterizes, creates
offscreen targets nor establishes GPU/device-loss recovery behavior.
Capabilities describe accepted semantic records with `rasterizes=false` and
offscreen size zero. Unconfigured/zero-size, suspended, wrong-epoch/size,
missing-resource and Busy presentation results are distinct.

`presentation_notice` preserves the exact borrowed frame key and separately
reported epoch for the existing stage acceptance gate. Export/upload completion
does not change input. For Accepted results it independently retains candidate
qualification and matching geometry until resolve or the last notice drop, so
caller frame drop and render completion may precede resolve. It retains no frame
commands/resources. Notice fields have read-only accessors and a retaining
`PresentationNotice::new` constructor. Independent analytical color/mask/crossfade
formulas live in `tests/support/color.rs`; production uses Peniko's typed color
conversion. Structural CPU replay remains a separate U02 reference.

Run `cargo test -p reel-render --locked -- --test-threads=1`. Tests use actual
Masonry image records for duplicate resource references, paused build retirement,
old/new content versions, owned Blob aliases, accepted upload and frame retention,
completion, slot generation, refusal and null presentation. See
[R01-INTERFACE.md](../reel-ui/R01-INTERFACE.md) and
[R01-REPORT.md](https://github.com/jhq223/reel/blob/355571b18bd4024914438a7ac6bd94ebf3c7826f/crates/reel-ui/R01-REPORT.md) for the integrated evidence.


## R02 optional real device profile

Enable `wgpu` for the two-phase production adapter. It consumes RenderFrame
without replay-to-Scene and uses Kurbo dashing, Lyon, Swash and wgpu. See
[R02-INTERFACE](R02-INTERFACE.md), [R02-REPORT](https://github.com/jhq223/reel/blob/355571b18bd4024914438a7ac6bd94ebf3c7826f/crates/reel-render/R02-REPORT.md), and the
[winit diagnostic launch command](../reel-player/README.md). Recording/null
remain feature-off semantic consumers; their simulated results are unchanged.
