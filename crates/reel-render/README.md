# Reel renderer

`reel-render` consumes the common UI `RenderFrame`. The optional `wgpu` feature
provides the GPU adapter; native window creation belongs to
[`reel-player`](../reel-player/README.md). The
[architecture](../../docs/engine-architecture.md) defines shared rendering semantics.

## Submission and ownership

[`RenderBackend`](src/lib.rs) separates configuration, owned resource upload,
borrowed presentation, retirement, bounded completion polling and suspension.
Rejected uploads return their original batch and tickets. Accepted uploads reserve
terminal capacity; pending bytes and retained residency have separate limits.

`RecordingBackend` models owned upload/frame residency and explicitly driven
completion. `NullBackend` reports skipped presentation. They exercise the same
ownership interfaces as the device adapter.

[`gpu::Renderer`](src/gpu/renderer.rs) prepares an opaque transaction before
submission. It owns its generation, frame leases, encoded uploads and budget
tickets. Dropping preparation releases these reservations. Submission transfers
them to in-flight ownership until observed queue completion. A transaction
belongs to the renderer that prepared it.

`WgpuBackend` acquires the surface, submits and presents before reporting
acceptance. Offscreen submission provides completion independently. Player input
acceptance uses the exact presentation notice; queue completion releases GPU
residency. An accepted CPU frame may be retained for expose repaint with its
existing leases.

## Budgets and rendering

Target, converted-resource and transient budgets are independent. Quality
selection uses actual adapter formats/sample counts and the whole frame's peak
simultaneous targets. Resolve and multisample attachments are charged separately.
Preparation reuses completed sibling targets after their reads are encoded;
active parents, masks and crossfade inputs retain exclusive storage. Reservations
remain live through completion. Refusal preserves the previously accepted view.

Composition uses linear premultiplied color. Groups apply opacity once to their
combined children; frozen-frame crossfade mixes complete results before surface
encoding. Clips and masks retain their own coverage. Recorded glyph placement is
consumed directly. Unsupported drawing records fail with their command location.

Pipeline/image/glyph caches belong to one renderer/device generation and use full
resource and raster identities. Prepared source leases and GPU residency retain
separate ownership. Application tickets account declared storage extents; driver
allocations and codec/tessellator internals retain their own allocation behavior.

## Surface lifecycle

Surface size/scale changes and resumption require a fresh surface epoch. Device
recreation also advances generation and rebuilds generation-bound caches.
Zero-sized or suspended surfaces skip acquisition. Surface loss permits
reconfiguration with retained CPU sources; device loss closes admission to that
generation. Shutdown drops the surface before its native window.

Temporary pressure retries after completion. Permanent refusal waits for a new
state or size invalidation. Expose repaint uses the last accepted CPU frame and
keeps its original input geometry. Actual target-device tests cover acquisition,
presentation and loss; headless recording/readback cover their own paths.

## Checks

```sh
cargo test --locked -p reel-render
cargo clippy --locked -p reel-render --all-targets --all-features -- -D warnings
```

The [GPU sources](src/gpu) document concrete preparation, caches, readback and
surface policies. Native diagnostic commands are in the
[player README](../reel-player/README.md#native-diagnostics).
