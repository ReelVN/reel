# R01 backend contract and semantic consumers

This crate depends on `reel-stage` and `reel-core`; it owns no window or native
handle. `RenderBackend` defines `capabilities`, `configure`, owned `upload`,
borrowed `render_present`, `retire`, bounded `poll` and `suspend`.

`SurfaceInfo` fixes physical size, scale, SurfaceEpoch, color encoding and alpha
mode. `UploadBatch` owns prepared resource leases, pixel/font `OwnedBytes`,
Scope and its vector-capacity ticket. Invalid metadata, overflow, unsupported
payloads or exhausted limits return the original batch before acceptance.
Accepted batches reserve resource, byte, metadata and terminal-event capacity.
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
does not change input. `color` supplies analytical sRGB-to-linear, premultiplying,
over, whole-group alpha/mask and crossfade reference formulas; it is not a
rasterizer. Structural CPU replay remains a separate U02 reference.

Run `cargo test -p reel-render --locked -- --test-threads=1`. Tests use actual
Masonry image records for duplicate resource references, paused build retirement,
old/new content versions, owned Blob aliases, accepted upload and frame retention,
completion, slot generation, refusal and null presentation. See
[R01-INTERFACE.md](../reel-stage/R01-INTERFACE.md) and
[R01-REPORT.md](../reel-stage/R01-REPORT.md) for the integrated evidence.
