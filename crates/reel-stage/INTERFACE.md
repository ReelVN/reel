# Stage public interface — U02 §15 and R01 §16

Current recording/render interface: [R01-INTERFACE.md](R01-INTERFACE.md), Issue
#25, architecture contract `ac004182`, on independently checked U02 `8dc2428`.
The remaining geometry/input descriptions below continue to apply. Historical
dependency information describes U02 before this extension.

Scope: crates/{masonry,masonry_core,tree_arena,include_doc_path,reel-stage}, required root manifest/lock and
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
  Hover/active callbacks may remove the target. Cursor rechecks current liveness;
  invalidated paths remain until a bounded rewrite pass sends exits to surviving
  ancestors. Callback invalidations are retained instead of being overwritten.
- `configure_input_path_storage(limit)` configures five reusable route arrays
  and an independent TreeArena lookup workspace of `limit` u64 slots.
  `input_path_storage_bytes_for(limit) -> Option<usize>` plans the allocation;
  `input_path_storage_bytes()` reports actual capacities plus workspace headers.
  The bridge reserves the complete Temporary ticket before configuration.
  One pointer bubble
  route is built once and also used by the focus check. Hover/active each use
  their own reused array, with old arrays retained through exit notifications.
- `QueryCtx::get`, `MutateCtx::get_mut`, `get_raw`, `get_raw_mut` and
  `child_is_stashed` return Option. `Button::child_mut` follows that Option.
  Layout's private assertions/mutation methods still require registered live
  children. QueryCtx stays Copy. Every own context binds explicit Live or a
  borrowed view plus validated record index before invoking widget code.
  WidgetRef.children enumerates selected topology and filters destroyed nodes.
- Shared and mutable safe-tree lookup iteratively fills a bounded circular path,
  then borrows direct children. Queries never allocate, grow or recurse. A
  try_lock lease protects the independent preallocated workspace and preserves
  Send/Sync. No lease/empty workspace/no_std uses 32 stack slots; an over-capacity
  path repeats bounded chunks instead of truncating or reporting a missing node.
  No route or still-leased path is overwritten. Relative depth D and capacity C
  cost O(D + D²/C); D<=C takes one parent walk. See the measured bound in README.
  Frozen
  bounds/clip reject candidates before live eligibility. Layer interception also
  checks current disabled/stashed/liveness and selected snapshot visibility.
  Layer behavior and capture acceptance require the selected record and every
  parent record to be unstashed. Live uses current eligibility. Ordinary transform
  changes do not re-hit-test capture. Terminal/exit cleanup keeps the old binding,
  including hidden records, until its one Cancel and exits have been delivered.
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
  the maximum export allowance before record/export and remains continuously
  charged until the last key/view lease drops. The same Arc allocation provides
  opaque candidate identity and its budget guard; no separate identity allocation
  or unbounded external identity table is added. Retired keys remain charged.
  This conservatively retains released scratch/export allowance; pool request and
  byte limits apply to all outstanding leases, including retired keys. Configure
  aggregate bytes for maximum-per-lease allowance plus host overhead.
- `record(FrameId) -> Result<RecordedSource,RecordError>` requires increasing
  issued IDs and moves the actual plan with its matching geometry/key.
  `RecordedCandidate` is a compatibility alias. Production `FrameBuild` visits
  the source directly; `scene()` is a lazy structural CPU reference. The base
  transform and External records are now supported using the forward convention
  described in R01. Cancellation/failure releases candidate eligibility; an
  unsubmitted source/build/frame drop cancels it. Accepted notices independently
  retain eligibility and matching geometry until resolve or final notice drop;
  keys alone do not retain eligibility. Key/view storage remains charged.
- `PresentationNotice::new(key, surface_epoch, outcome)`, with read-only accessors and
  `PresentationOutcome::{Accepted(FrameId),Skipped,SurfaceUnavailable,Error}`.
  `resolve` checks the renderer's FrameId, reported SurfaceEpoch, exact opaque
  candidate identity, current epoch and acceptance order. Wrong/foreign/duplicate
  notices cannot change the view. Failed outcomes retire only their candidate.
  The retaining constructor cannot revive cancellation. After an Accepted notice
  is created, caller frame drop and graphics completion may precede resolve;
  the notice retains no frame commands/resources. See [R01 ownership](R01-INTERFACE.md).
- `clear()` and increasing `replace_surface(epoch)` exit through the old view
  before releasing it; externally retained geometry stays charged. The bridge
  exposes immutable root inspection, narrow `edit_widget`, and pointer/text/
  window event dispatch; it does not expose mutable RenderRoot access that could
  bypass acceptance.

StageGraph owns NodeId allocation, generations and author-node mappings (§6).
D01 identity.rs supplies FrameId/SurfaceEpoch, not an author NodeId type. U02
uses non-reused Masonry WidgetIds for the private widget tree; it introduces no
parallel NodeId allocator or StageGraph. R01 adds prepared resource versions,
bounded frame export and ExternalDraw. Author Slider/DSL and GPU/device support
remain separate tasks. Bridge tickets cover geometry and fixed routes/registry;
R01 independently accounts output/resource/backend ownership and measures opaque
producer Scene/cache residency.

No author language syntax is added. Both guides now state R01's Rust-only
implementation scope. Verification and measurements are in README.md,
R01-REPORT.md (historical evidence links) and `target/reel-artifacts/` for new runs. Historical U02 32-bit checks are separate from
native-library linking, target execution and real-device acceptance.
