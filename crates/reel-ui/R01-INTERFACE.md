# R01 interface — Issue #25 / architecture §16

Contract: `ac00418231ddc16ec0b90b2181cd77138b3f6e49`; dependency U02:
`8dc242891be199ef2e6da7bc88a6ffdaf80cc403`. The ordinary `imaging 0.0.1`
record API is consumed directly. No imaging source fork, shaping, tessellation,
GPU implementation, window handle, author DSL or second author-node allocator
is introduced. `reel-ui` owns the widgets, geometry, bridge, frame and resources, and reads reel-core;
`reel-render` reads `reel-ui/frame`, `reel-ui/resources` and core; native handles remain in renderer/player. Native device verification is described in the R02 report.

## Recording and compilation

`PresentationBridge::record(FrameId) -> Result<RecordedSource, RecordError>`
moves the actual `VisualLayerPlan`, matching `PresentedGeometryView` and exact
`CandidateKey` from one Masonry record operation. `RecordedCandidate` remains a
type alias for U02 consumers. `plan()`, `geometry()` and `key()` borrow the
frozen record. The compatibility `scene()` lazily allocates a second replay
Scene **only when explicitly called by structural CPU reference tests**; it
omits External draws and is not the production consumer.

`FrameBuild::new(source, snapshot, FrameInfo, BuildLimits, &BudgetPool)` consumes
one source and resource snapshot. It reserves the full output/scratch allowance
before allocating. `step(StepBudget)` advances borrowed public
command/clip/group/mask/draw/context data and returns `BuildProgress`.
`finish(self)` publishes a complete immutable `RenderFrame` or returns
`Incomplete`. `cancel(self)`, a build error or an epoch change releases candidate
eligibility. Source/build/frame drop cancels an unsubmitted candidate; an Accepted
notice independently retains eligibility after submission. Retained keys and geometry remain charged
until their final lease drops. No partial frame is exposed.

`BuildLimits { max_bytes }` bounds output and scratch storage. Fragments, commands,
mask definitions and nesting have no separate count or depth limit. Command storage
uses actual source counts; traversal and mapping capacities grow with the scene and
are charged before allocation. Completed mask cursors release their scratch charge.
`StepBudget` separately bounds committed commands, work units and byte work.
There is no aggregate work cutoff: larger scenes continue across more slices.
All payload cursors survive pause: path elements, stroke dashes, gradient stops,
glyphs, normalized variation coordinates, filters and each UTF-8/context/source
string byte. Mapping and command-header work is prepaid across slices before
lookup, validation or allocation. Payload-capacity reservation credit can also
accumulate across slices; element copying has a separate byte charge.
`minimum_units` / `minimum_bytes` identify the next indivisible element when
the supplied slice cannot advance it. A caller must permit that minimum.
Capacity allocation, source cleanup and widget recording are atomic operations;
work credit does not establish a wall-clock deadline for them. Their real costs
and overlapping source residency are measured in [R01-REPORT.md](R01-REPORT.md).

`BuildError { at: RecordLocation, kind }` reports fragment, local command,
mask depth and owning WidgetId. Missing resources include a Blob ID lookup hint
or External token. Structured source file/line/column and diagnostic contexts
are retained in the completed frame. Invalid/nonfinite records, exhausted byte
storage, unrepresentable frame IDs and unsupported records abort the whole candidate.
The painter's command ranges identify ordinary Inline children as well as layer
owners. A range cursor advances only to the next contiguous range during a
prepaid command commit or fragment advance; setup seeds the first owner by one
indexed read. Errors after mapping prepayment report that command's owner,
with no diagnostic scan or late binary lookup. Payload continuations keep the
same owner. Generated clips and debug/inspector commands
also record their actual owner. Mask commands inherit their defining owner.

## Portable stream

`RenderFrame` is an immutable Arc-backed lease with frame/epoch, logical and
physical sizes, matching geometry, commands, mask definitions and resource
snapshot. `clone()` creates ownership for an asynchronous borrowed submission
without duplicating payloads. `accounted_bytes()` includes conservative reserved
scratch capacities; `reserved_bytes()` is the original maximum ticket, retained
continuously through the last frame clone.

`FrameOp` preserves Fill/Stroke/GlyphRun/BlurredRoundedRect, Push/PopClip,
Push/PopGroup, Push/PopContext and External. kurbo/Peniko shape, brush, gradient,
stroke, sampler and composite values are retained. Glyph IDs/positions, font
size, font collection index, hinting, transforms, style and variation coordinates
are copied from the original `reel-text` result without reshaping.

`CommandOrigin` identifies `(source, local ID)`; local IDs alone are never a
global identity. Source identity and local MaskId key the byte-budgeted definition
map. Definitions remain local, while each `MaskUse` carries its transform.
The ordered main stream keeps clip/group/context state across fragments;
each nested mask has its own explicit cursor and growing validation stacks.
Normal cross-fragment nesting is accepted. Crossed clip/group lifetimes return
localized `UnsupportedRecord`, because imaging does not define their pixel
meaning; underflow and unclosed lifetimes return `InvalidRecord`.

`FrameGroup` carries guaranteed owner/effect space metadata recorded at the
Masonry paint/append boundary, filters, composite, clip, mask and conservative
ink bounds. Rect/path control bounds include stroke/miter and one physical
pixel of AA allowance using frozen DPI. Finite bounds overflow, local-mask
sampling footprints, Gaussian cutoff, unknown glyph outlines and filter output
use `InkBounds::Unbounded`, requiring
the backend to clamp to its actual target. They never substitute a layout box
for ink bounds or guess the effect owner from a child draw.

## Prepared resources

`ResourceVersion` reuses core `AssetKey` + `ContentVersion`. Preparation owns
that version; frame export never hashes, decodes or reparses a blob per draw.
`ResourceHandle { registry, slot, generation }` rejects foreign registries and
old slot generations. Image/font lookup first searches sorted Blob IDs, then
checks the actual backing Arc and image/font metadata. Blob ID collisions do
not establish object or content equality. One source object cannot have two
ambiguous versions in a frozen lookup table.

`ResourceRegistry::new(max_resources, max_bytes)` charges fixed slots,
descriptor/identity storage, version-string capacities and owned Blob headers
before allocation. `register_image(ResourceVersion, OwnedBytes, size, format,
alpha_type, encoding)` and `register_font(ResourceVersion, OwnedBytes)`
attach the full core byte-capacity ticket to the Blob backing allocation.
Font resources own the complete font file or collection. `FrameGlyphRun.font_index`
selects its face; all faces share the same resource, bytes and lifetime.
Producer/widget/cache Blob clones keep that ticket and header reservation even
after frame, registry and simulated graphics retirement. Imported opaque
`register(version, ResourceData, BudgetTicket)` charges the declared retained
extent; its producer's inaccessible allocation/cache capacity remains a separate
accounting domain. External uses a token and a zero-byte extent ticket.

`acquire(handle)` returns an owned `ResourceLease`; `freeze(&handles, &pool)`
creates one bounded, prepaid lookup table holding the selected leases. The
entire selected set remains leased through the frame, even if some entries are
unused. Duplicate or ambiguous table entries are rejected.
`retire(handle)` denies new leases and removes the registry's own reference.
Existing build/frame/upload leases remain valid for submission. A slot can
be reused only after the prior resource's last strong lease disappears, and
its complete generation is incremented. Backend completion independently
controls backend residency; raw producer aliases independently control source
Blob memory. Coordinate registry and backend retirement explicitly.
Before reserving a replacement descriptor, the registry releases its own expired
weak ticket for the selected slot. Genuine external weak aliases continue to
retain that allocation's ticket; quota exhaustion returns `Bytes` until they drop.

## Masonry changes and presentation

The same forward local-to-window convention applies to the base and overlays.
Full `compose_local_transform()` includes origin and scroll. Finite singular
transforms remain drawable without inversion; input's inverse mapping is empty.
Debug bounding boxes already use window coordinates and are emitted in identity
fragments, with baselines transformed forward into the same space. Open painter
scopes continue across those fragments, including under singular transforms.
External records retain opaque token, WidgetId, content-local bounds and forward
content transform in exact `pre → own clip → external → children → pop clip →
post` order. Cached pre/paint/post callbacks retain the prior mode/token until
explicitly reset to Inline. The producer change record and complete licensed
local changes are under [UI provenance](ORIGIN.md).

Successful export or upload never installs input geometry. Only
`PresentationBridge::resolve` with the exact candidate identity, FrameId,
reported SurfaceEpoch, current epoch and valid acceptance order can accept
`PresentAccepted`. Clear/failure/cancellation retain U02's old-view exit and
capture cleanup behavior. Backend completion is a separate event.

`PresentationNotice::new(key, reported_epoch, outcome)` replaces struct literals;
read-only `key()`, `surface_epoch()` and `outcome()` keep ownership consistent.
Create Accepted notices only from an actual presentation result. While the exact
candidate remains eligible, the constructor acquires a qualification guard and
the matching geometry, without retaining the source plan or frame resources.
Keys alone cannot revive a cancelled candidate. Notice clones share that view
and each retain qualification until `resolve` consumes them or the last notice
drops. `render_present → notice → caller frame drop → complete_render → resolve`
is valid. GPU completion can release frame/output resources before input acceptance.
Nonaccepted notices do not retain qualification. Explicit build cancellation,
failure, clear and epoch replacement still invalidate all notices.

The backend contract, upload ownership and simulated consumers are documented
in [reel-render/README.md](../reel-render/README.md). Verification and source
residency evidence are in [R01-REPORT.md](R01-REPORT.md).
