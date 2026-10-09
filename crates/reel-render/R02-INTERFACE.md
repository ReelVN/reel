# R02 device contract (implemented portable device profile)

Scope: `reel-render` optional wgpu adapter and `reel-player` assembly; stage/core
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
sampling, color glyph layers, glyph stroke/embolden/local glyph transform and
sizes above 256. Font IDs, collection index, variation coordinates, hint and
position are retained without shaping. This explicit profile does not remove any
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
