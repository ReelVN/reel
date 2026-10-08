# U02 interface draft — architecture §15, contract 31b9430

Scope: third_party/masonry, crates/reel-stage and required root manifest/lock.
Dependencies: PR #7 (31b9430, experiment ccd0547), D01 (c03226a).
Task branch combines both; PR targets PR #7 to keep the source closure reviewable.
D01 must be integrated before U02 can be rebased onto the shared production base.

Masonry API (no reel-core dependency):

- `InputGeometrySource::{Live, Presented(Option<Arc<PresentedGeometryView>>)}`.
- `GeometryLimits { max_nodes, max_bytes, max_path }`; bounded export returns
  `Result<PresentedGeometryView, GeometryError>` before partial publication.
- `RenderRoot::record_presented_geometry(limits)` produces the real
  `VisualLayerPlan` and compact geometry after the same rewrite/compose/record.
- `RenderRoot::set_input_geometry_source(source)` completes old-view exits and
  invalid capture cancellation before new-view entries and stationary cursor.
- `QueryCtx::get`, `get_raw`, `get_raw_mut` return Option. A context carries an
  explicit borrowed Live selection or a verified presented record index.
  WidgetRef.children follows selected topology and filters destroyed nodes.
- Snapshot is contiguous geometric records + child indices + sorted ID index;
  every registered node (including stashed) is represented. No WidgetState,
  widget/property/layout cache/action or per-node ancestor vector is retained.
- One bounded reusable route per dispatch, shared by focus check and bubbling.
  Pointer hover/active/cursor use selected view; nonpointer passes are Live.

Reel minimum acceptance bridge (no render/player dependency):

- `PresentationBridge` owns RenderRoot, SurfaceEpoch, monotonic FrameId,
  bounded candidate count/snapshot bytes, and presented/candidate reservations.
- `RecordedCandidate` exposes an opaque `CandidateKey` plus actual owned Scene
  (complete VisualLayerPlan replay) and immutable geometry from the same record.
- `PresentationNotice { key, outcome }`; `Accepted` checks bridge/candidate
  identity, FrameId, SurfaceEpoch, and ordering. Skipped/Unavailable/Error do
  not switch geometry; duplicate/stale/foreign notices cannot switch it either.
- Clear/surface replacement invalidates candidates and cleans input through the
  old view. Accepted geometry remains charged until its last lease is dropped.
- No StageGraph, author NodeId allocator, resource registry, GPU renderer,
  ExternalDraw, image slider or DSL implementation is claimed here.

Author behavior remains the approved §15 behavior. The lead owns creator guides.
Performance evidence will report actual exported nodes, retained capacities,
record/snapshot/input p95 and capacity exhaustion/recovery. 32-bit compilation
is separate from runtime/device verification.
