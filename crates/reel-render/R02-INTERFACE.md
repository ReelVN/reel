# R02 device contract (implemented portable device profile)

Scope: `reel-render` optional wgpu adapter and `reel-player` assembly; UI/core
remain portable. Started from R01 411cc7531431f35fbd43a614476815e22cd58f21;
synchronized the exact accepted painter work-error repair
9578cb3991d62a5dffb6a8580a1a6d1a850230cc. No VM execution.

The adapter consumes RenderFrame directly. Native window ownership and Surface
stay in player assembly. Device generation is monotonic and independent of
SurfaceEpoch. Recreating a device invalidates every GPU cache entry; resizing or
resuming requires a new surface epoch and invalidates input candidates through
PresentationBridge. A queue completion never constitutes presentation acceptance.
Only successful surface acquisition, submission and present permit Accepted.
Offscreen submission has no Accepted result. Completion retains the exact frame,
resource leases and allocations until the queue callback is observed.

Quality is negotiated from actual adapter format usages/flags and configured
budgets: linear premultiplied Rgba16Float/4x preferred, explicit Rgba16Float/1x
or linear Rgba8Unorm/1x fallback. Targets account resolve plus multisample
attachments separately, including nested groups, mask coverage, frozen crossfade
inputs, and overlapping submissions. Default target budget 128 MiB. Resource,
transient mesh/upload and target budgets are independent; failures preserve the
last accepted view. No unbounded cache or hidden target reuse across generations.
Cache identity must include full ResourceHandle, content version and device
generation; glyph identity additionally includes all rasterization parameters.

Source-over composition uses linear premultiplied values. Groups isolate children
and apply opacity once at group end. Clips and alpha/luminance masks are coverage
textures; nested scopes preserve inherited coverage. Frozen-frame crossfade mixes
two complete linear results before surface encoding. Existing glyph positions
are consumed without shaping. Unsupported brushes, composites, filters or external
records fail the whole frame with CommandOrigin/RecordLocation, never disappear.
The full architecture scope remains required, even when a profile rejects a record.

Device loss stops admission and invalidates generation-bound allocations. Surface
loss does not imply device loss: reconfigure under a new epoch, preserve CPU
sources and re-record. Suspended/zero-sized windows perform no acquisition.
Shutdown stops admission, drains or cancels terminal obligations, drops surface
before window. GPU completion and Accepted must be independently tested.

Verification: actual readback against analytical color, whole-group opacity,
nested clips/masks, glyph/image sampling, queue delays/retirement and recovery;
existing R01/U02, fmt, strict Clippy, debug/release and feature-off checks.
CPU/software adapter, physical GPU and window evidence are reported separately.

## Concrete two-phase adapter API

The optional `gpu::Renderer` deliberately separates `prepare(RenderFrame)` from
`submit(Prepared)`: surface acquisition/presentation is in the assembly crate,
so an offscreen renderer cannot manufacture PresentAccepted. The earlier
RecordingBackend/NullBackend trait and its simulated events remain unchanged.
`Prepared` is opaque outside the adapter. It owns a renderer identity, generation,
frame leases, command encoder and budget tickets. Its image copies are encoded
into the same submission before their draws; there is no immediate queue write
that could escape a rejected preparation. The image lookup cache is local to this
prepared submission and keys the complete ResourceHandle; the exact frame lease
retains ResourceVersion. No persistent glyph atlas or cross-frame cache exists in
this first profile. This avoids cache invalidation aliases at the cost of repeated
rasterization/uploads, which remain optimization work rather than a performance claim.

Preparation reserves a submission slot as well as each target/resource allocation.
`Busy` is temporary admission pressure; `Pressure` is shared residency held by
other prepared/in-flight submissions. `Budget` is a permanent limit of this
preparation itself. Per-preparation live quotas are checked before shared quotas,
including readback storage and mesh scratch. All partial reservations release
on refusal. The player retains latest state after temporary refusal and retries
only when `poll()` observes completion. A permanent refusal stops preparation
until a new state/size invalidation. Neither refusal updates accepted input geometry.
OS `RedrawRequested` is a separate content-invalidation obligation. After successful
presentation it acquires/submits/presents again even with no model change. A blocked
latest model is never recompiled just because of expose: the player retains at most
one already accepted CPU RenderFrame (with its existing budgeted resource/geometry
leases) and repaints that exact frozen frame. Repaint does not issue a new bridge
acceptance or install latest model geometry. Temporary repaint pressure waits for
completion too; successful repaint preserves the model's permanent block. The
retained CPU frame is replaced on new acceptance and cleared on epoch change,
suspend or shutdown. GPU allocation residency still ends at observed completion.
Dropping a preparation cancels it and releases its tickets. Submission moves those
tickets and frame leases into an in-flight entry; only observed queue completion
releases them. `poll()` returns completed submission IDs once, at most the configured
in-flight bound. Foreign-renderer preparations are rejected even if their caller
supplied device generation happens to match. Retired source leases already in a
frame remain usable; GPU residency has no independent persistent cache lease and
ends on completion. The R01 registry separately prohibits new source leases.

Limits: default two prepared/in-flight submissions, nesting 32 (maximum 64),
128 MiB targets, 128 MiB converted resource/staging data, 32 MiB transient data.
Each domain also has a 65,536-ticket cap. RGBA16 texture upload accounting includes
f32 conversion storage, padded CPU staging, GPU staging and texture storage.
The target budget charges every full-viewport scope, clip coverage, mask result
and crossfade input, including 4x attachment plus resolve. Exhaustion refuses the
whole frame; there is no implicit per-group quality change or cache eviction.
Application-accounted extents do not measure driver allocator/pipeline memory or
prove hard bounds on private Lyon/Swash scratch. Those upstream internals and
persistent mesh/glyph caches require further profiling before an engine-wide
hard memory/latency guarantee. Blocking diagnostic readback reserves buffer/output
scratch; its returned pixel vector is caller-owned diagnostic memory.

The implemented profile draws source-over solid fills/strokes, bilinear padded
RGBA8 images with linear conversion, monochrome filled Swash outlines from Parley,
whole-group alpha, geometric clips, nested alpha/luminance masks and frozen-frame
crossfade. Rejected with RecordLocation: gradients, non-source-over blends,
filters/blur, External producers, dashed strokes, non-bilinear/repeat image
sampling, color glyph layers and glyph stroke/embolden/local glyph transform.
Glyph raster extents are admitted against the current device's texture dimensions
and the remaining resource upload budget, with no separate font-size ceiling.
Font resources retain the complete file Blob, including every face of a TTC;
the shaped glyph run independently supplies its collection index. Font IDs,
collection index, variation coordinates, hint and position are retained without
shaping. This explicit profile does not remove any
architecture §16 feature from the full engine scope.

Stroke width must convert to a finite, nonnegative f32 without nonzero underflow.
Miter limit must be at least 1 in the original f64 and finite after f32 conversion,
for every join including Bevel/Round. R01's legal `[0, 1)` miter values are rejected
here with their drawing or opening clip `RecordLocation`; there is no clamp.
Replay root/scopes and recursive mask roots count toward `max_depth`. Exceeding
it returns permanent `DepthLimit` at the responsible command (the opening group
for an empty mask); mismatched, underflowing or unclosed scopes return permanent
`InvalidRecord`. These cannot be confused with admission `Busy`.

Every surface configuration/recreation and device recovery freshly queries the
current adapter/surface pair and rebuilds the SurfacePass for its selected format.
The diagnostic profile explicitly requires an sRGB surface format, opaque alpha,
FIFO presentation and render-attachment usage. An incompatible set is refused;
old configuration is never reused. Surface epoch/candidate invalidation precedes
negotiation, including failure; a recreated device advances generation independently.

## Renderer-local pipeline reuse (Issue #32)

`prepare(&self)` and `crossfade` lazily reuse the complete paint/composite
Pipelines in exactly three instance-owned slots: Rgba16Float/4x,
Rgba16Float/1x and Rgba8Unorm/1x. Slot identity uses actual format and samples;
the diagnostic `degraded` flag is not an identity. Quality negotiation and all
budgets remain unchanged. A per-slot mutex serializes first construction under
concurrent preparations; one additional instance-owned cold-construction mutex
serializes the complete error-scope transaction across all three slots. Warm
lookup/replay does not acquire that construction mutex or manipulate scopes;
replay uses an Arc after releasing the slot mutex. Unused
tiers allocate no GPU pipeline objects. Each initialized slot creates two
shaders, two bind-group layouts, two pipeline layouts, two render pipelines and
one sampler once. SurfacePass remains independently rebuilt after negotiation.

Slots belong to one Renderer/Gpu, never to a global generation-number map. A
fresh Renderer starts empty even with the same caller-supplied generation.
Prepared ownership validation still rejects old/foreign preparations. Slots
hold only immutable pipeline objects, no RenderFrame, source lease, target,
upload, texture, bind group or budget ticket. Valid pipeline construction may
remain cached after a later record/budget refusal, but every partial preparation
reservation still releases. Drop Prepared and observed submission completion
return the same application usage baseline while initialized slots remain.
Renderer drop drains pending work as before and drops its slots.

Gpu's sticky loss/error state is checked before lookup, after acquiring the
initialization mutex and cold-construction mutex, and after construction before
publishing a slot. Construction owns nested Validation, Internal and OutOfMemory
scopes and pops all three in reverse order before returning. Only three confirmed
error-free results and Gpu::check permit publication. Captured construction errors
enter Gpu's existing sticky Driver state; DeviceLost retains priority. A
construction fault is returned without publishing; a later asynchronous driver
fault denies cached hits, preparation, submission and readback through the
existing checks. Slots in a lost/faulted Renderer are unusable and are released
when that instance drops. Driver-private pipeline memory remains outside the
measurable application budgets; the three-slot bound is an object-count bound,
not an engine-wide memory or latency guarantee. This change adds no white-texture,
atlas, mesh/image/glyph reuse or scene-wide quality negotiation.

Caller error scopes are reachable through `Renderer::gpu().device` and may
normally enclose prepare/crossfade, including a group of concurrent preparations.
Inner construction errors are reported as Driver by Renderer, even when a
matching outer scope exists; outer scopes retain their own previously captured
errors. Caller-captured errors from other raw-device operations remain the
caller's responsibility and do not automatically enter Gpu's uncaptured state.
wgpu 26 native scopes share one device stack. The caller must pair its push/pop
and avoid interleaving stack mutations with Renderer's construction transaction
or other callers' scope transactions; private locks cannot coordinate arbitrary
public-device push/pop from another thread. Parallel Renderer preparations are
still supported, with cold construction serialized across tiers.

The native wgpu-core 26 backend returns immediately ready scope-pop futures.
Confirmation polls each future once and never waits for submitted GPU work. This
is not a WebGPU Promise guarantee: an unresolved pop causes a permanent Device
refusal, drops the attempted Pipelines and publishes no slot. Supporting a backend
that requires asynchronous confirmation needs a separately agreed asynchronous
preparation contract; it is not implemented by blocking `prepare(&self)`.

## Preparation-local white texture (Issue #36 / R04)

Each prepare/crossfade owns one Context with an initially empty white-image
slot. Its first solid fill/stroke or geometric clip reserves the existing
resource upload ticket and encodes one RGBA16Float 1x1 white upload before its
draw in the same encoder. All subsequent solid/clip uses, including recursive
masks and both crossfade inputs, share that image within this Context only.
An unused slot creates/uploads nothing. No slot or image is stored in Renderer,
Gpu or Pipelines, and a later transaction must reserve/upload independently,
even after an earlier preparation was dropped without submission.

The existing upload_cost([1,1]) remains 536 resource bytes: 16 bytes of f32
input, 256 bytes each of CPU/GPU padded staging, and 8 bytes of texture extent.
This conservative application charge is held once per using transaction, not
once per draw; target/transient quotas and every other resource path are
unchanged. It does not measure driver allocator granularity. Publication follows
successful upload encoding. Refusal/cancellation drops the Context/encoder and
all its reservations. Prepared carries the ticket through submit into Inflight;
only observed completion (or Renderer teardown after completion/device loss)
releases residency. The encoder/queue own the actual wgpu resource references
after the Context slot drops. Device/foreign-preparation checks and R03 pipeline
scope confirmation remain unchanged. No quality, public API or atlas change.
