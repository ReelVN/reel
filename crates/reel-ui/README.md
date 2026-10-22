# Reel UI

`reel-ui` owns the platform-independent widget tree, common author Stage,
layout/input/paint passes, presentation acceptance, portable frames and prepared
resource leases. Text shaping and editing use [`reel-text`](../reel-text/README.md).
Native rendering and windows belong to [`reel-render`](../reel-render/README.md)
and [`reel-player`](../reel-player/README.md). [Source and license](ORIGIN.md).

| Module | Responsibility |
| --- | --- |
| [stage](src/stage) | Common author nodes, properties and logical ownership |
| [core](src/core), [passes](src/passes) | Widget contexts, events and traversal |
| [app](src/app), [bridge](src/bridge.rs) | Layers, candidate identity and accepted geometry |
| [layout](src/layout), [properties](src/properties) | Measurement and shared styles |
| [widgets](src/widgets), [theme](src/theme.rs) | Controls and default appearance |
| [frame](src/frame), [resources](src/resources.rs) | Portable recording and budgeted resources |

## Input and presentation

`RenderRoot` supports live geometry and frozen presented geometry.
`PresentationBridge` starts without an accepted view; `record(FrameId)` records
one actual plan, matching geometry and opaque candidate key. The resulting
`RecordedSource` follows compilation and submission as one candidate.

Only `resolve` of an accepted notice with the exact key, FrameId, SurfaceEpoch
and valid order installs input geometry. Build/upload completion and GPU completion
have their own lifetimes. An accepted notice retains qualification and geometry
until consumed or dropped, allowing its frame resources to finish earlier.
Clear, cancellation and epoch replacement invalidate candidate qualification.

Pointer routes use the selected view while current liveness, disabled/stashed
state and modal eligibility remain authoritative. A view change delivers old
exits and capture cancellation before new entries. Cleanup retains the old route
until delivered. `stop_propagation` ends ancestor delivery; `set_handled` also
consumes the event. Route and safe-tree lookup scratch grows before publication
within its budget and is reused during input.

LayerStack registers its direct layer roots for ordered capture. Nested stacks
use the same accepted geometry and focus routing. Popup layers share the input
state machine; dismissal follows the active modal scope.

## Frames and resources

`FrameBuild` advances recording in command/work/byte slices and exposes a complete
immutable `RenderFrame` at finish. Payload cursors and source locations survive
pauses. Reported minimum work/bytes identify the next indivisible step. Build
errors retain command origin and owning widget. Allocation, widget recording
and source destruction remain their actual atomic operations.

Output, scratch, geometry, routes and registry capacities are reserved before
allocation. Candidate/frame/view leases retain their reservations until final
release, including leases kept after retirement. Byte budgets cover actual
storage growth; scene size and depth follow the recorded content.

`ResourceRegistry` keys prepared data by exact content version and generational
handle. Image/font payloads retain their byte tickets through Blob aliases;
fonts share their whole file across collection faces. Frozen resource snapshots
own the selected leases. Retirement denies new leases while existing
build/frame/upload leases remain valid; slot reuse waits for their final release.
Backend residency is independently retained until submission completion.

Portable frames preserve drawing, clips, groups, masks, context and external
records with forward transforms. Glyph positions come from the existing text
layout. Source identity qualifies local command and mask IDs; invalid scopes or
unsupported records fail the candidate at their recorded location.

## Checks

```sh
cargo test --locked -p reel-ui
cargo clippy --locked -p reel-ui --all-targets -- -D warnings
cargo run --locked -p reel-dev -- audit-ui
cargo doc --locked -p reel-ui --no-deps
```

Public API and widget documentation live beside their Rust declarations; shared
concepts live in [src/doc](src/doc). The [creator guide](../../docs/guide/en.md)
uses the same Stage through the engine and player.
