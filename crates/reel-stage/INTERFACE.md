# U02 public interface — architecture §15 / contract 31b9430

Scope: third_party/masonry, crates/reel-stage, required root manifest/lock and
this task's verification. Dependencies: PR #7 (31b9430, experiment ccd0547) and
D01 (c03226a). Task branch combines both; draft PR #12 targets PR #7. Integrate
both dependencies before rebasing onto the shared production base.

## Masonry (no reel-core dependency)

- `InputGeometrySource::{Live, Presented(Option<Arc<PresentedGeometryView>>)}`.
  Presented(None) has no geometric hit; standalone Masonry defaults to Live.
- `GeometryLimits { max_nodes, max_bytes, max_path }`: registered-node count,
  export heap peak and maximum depth. `GeometryError::{Nodes,Bytes,Path}` refuses
  publication. Bytes include snapshot vector capacities and temporary child-ID
  heap arrays; the native call stack is separate.
- `RenderRoot::record_presented_geometry(limits) -> Result<(VisualLayerPlan,
  PresentedGeometryView),GeometryError>` executes real rewrite/layout/compose/
  paint and exports matching geometry. `capture_presented_geometry` exports
  without acceptance; callers must use the record operation for candidates.
- `set_input_geometry_source(source)` completes old-view exits and invalid
  capture Cancel before new-view entries/stationary cursor. Shared ancestors
  receive no duplicate notifications; surviving capture remains active.
- `configure_input_path_storage(limit)` configures five reusable route arrays;
  `input_path_storage_bytes()` reports actual capacities. One pointer bubble
  route is built once and also used by the focus check. Hover/active each use
  their own reused array, with old arrays retained through exit notifications.
- `QueryCtx::get`, `MutateCtx::get_mut`, `get_raw`, `get_raw_mut` and
  `child_is_stashed` return Option. `Button::child_mut` follows that Option.
  Layout's private assertions/mutation methods still require registered live
  children. QueryCtx stays Copy. Every own context binds explicit Live or a
  borrowed view plus validated record index before invoking widget code.
  WidgetRef.children enumerates selected topology and filters destroyed nodes.
- Snapshot: contiguous geometric records + child indices + sorted WidgetId
  index, including stashed registrations; no WidgetState, property/layout cache,
  widget/action or per-node ancestor vector. Insets/clip use border coordinates;
  baselines are resolved content baselines. Current disabled/stashed/liveness
  remain live. `with_retention_guard` ties host accounting to every view lease.
- All constructor sites in event/update/contexts/widget_ref/render_root/anim
  and action/accessibility/compose/layout/mutate/paint are migrated. Nonpointer
  entry points are explicitly Live; pointer updates/RawCtx inherit selection.
- `paint_layer_mode` is retained across cached pre/paint/post callbacks.

## Reel minimum acceptance bridge (no render/player dependency)

- `PresentationBridge::new(root, epoch, BridgeLimits)` takes ownership and starts
  Presented(None). Limits cover nodes, export bytes/depth, candidate count,
  aggregate snapshot bytes and fixed route/registry storage. A D01 Temporary
  ticket is reserved before fixed storage allocation; a Snapshot ticket reserves
  the maximum export allowance before record/export and then charges actual
  retained storage. Export peak can be stricter than final residency.
- `record(FrameId) -> Result<RecordedCandidate,RecordError>` requires increasing
  issued IDs. Candidate exposes `key()`, borrowed `scene()` and `geometry()`.
  Scene is actual complete VisualLayerPlan replay, with re-registered Scene IDs
  and group/clip/context scope preserved. External placeholders are explicitly
  refused, since U02 does not implement ExternalDraw.
  An identity base layer root is required: fixed upstream paint.rs strips its
  transform while input retains it. A nonidentity root returns
  UnsupportedRootTransform; stage transforms belong on children of that root.
- `PresentationNotice { key, surface_epoch, outcome }`, with
  `PresentationOutcome::{Accepted(FrameId),Skipped,SurfaceUnavailable,Error}`.
  `resolve` checks the renderer's FrameId, reported SurfaceEpoch, exact opaque
  candidate identity, current epoch and acceptance order. Wrong/foreign/duplicate
  notices cannot change the view. Failed outcomes retire only their candidate.
- `clear()` and increasing `replace_surface(epoch)` exit through the old view
  before releasing it; externally retained geometry stays charged. The bridge
  exposes immutable root inspection, narrow `edit_widget`, and pointer/text/
  window event dispatch; it does not expose mutable RenderRoot access that could
  bypass acceptance.

StageGraph owns NodeId allocation, generations and author-node mappings (§6).
D01 identity.rs supplies FrameId/SurfaceEpoch, not an author NodeId type. U02
uses non-reused Masonry WidgetIds for the private widget tree; it introduces no
parallel NodeId allocator or StageGraph. Full resource-version conversion,
renderer Scene/blob residency, ExternalDraw, author Slider, DSL and GPU/device
support remain separate tasks. This bridge's tickets cover geometry and its
fixed routes/registry, not the entire widget runtime or graphics resources.

No author language/API behavior beyond the already approved contract is added.
The lead maintains creator guides. Verification commands and actual measurements
are in README.md and evidence/. 32-bit compile checks are separate from native
library linking, target execution and real-device acceptance.
