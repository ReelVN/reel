# Reel production Masonry closure (U02 / #11, R01 / #25)

Upstream: https://github.com/linebender/xilem/tree/271a27a6d4a930f7878d404f9014e3c50a3a9b88
License: Apache-2.0; original LICENSE and file copyright headers retained.
`UPSTREAM-SOURCE.sha256` records original bytes before local changes.

Imported: masonry_core source/docs, tree_arena, include_doc_path procedural macro;
Masonry Button/Image/Slider and Button's Label dependency, their five property
modules, selected default theme and referenced screenshots. No winit, GPU,
Xilem, virtual list, examples or nested Git repository. Unrelated widget modules
and screenshot harness tests are omitted; real-control regression tests live in
reel-stage. No Slider behavior/configuration API is changed by U02.

Local manifest changes: four members of the root Reel workspace, selected modules/theme,
no backend/testing features; exact ordinary dependency versions and root lock.
The production geometry/context/input changes are listed below.

Production local changes:

- Compact app/presented_geometry.rs exports all registered geometry and topology;
  no full state clones or ancestor arrays. Explicit source/view/index bindings,
  bounded reused routes, export scratch accounting and host retention guard.
  Traversal borrows direct arena children, avoiding temporary per-node safe-tree
  lookup paths. Registered node/byte bounds are checked before traversal.
- Context constructors in every pass explicitly select Live or validated borrowed
  pointer geometry. Optional child lookups and selected WidgetRef.children.
  Button::child_mut and the internal layer-root lookup propagate Option.
- Pointer event terminal delivery marker keeps capture active through Up/Cancel;
  removal delivers one Cancel only when no terminal callback has begun.
- update.rs diffs old/new paths, retains old view for exits, cancels invalid
  capture, keeps valid capture and updates the stationary cursor in the new view.
  Stashed changes now request pointer reconciliation immediately.
  Rechecks target liveness after notifications that may remove it, retains
  invalidations and reconciles surviving ancestor flags in the bounded rewrite.
- Layer pointer interception obeys current disabled/stashed/liveness and the
  selected snapshot's effective stashing, including parent records. Capture
  acceptance uses the same snapshot visibility check without re-hit-testing its
  geometry; terminal/exit cleanup retains the old binding independently.
- Safe tree_arena shared/mutable lookups use nonrecursive bounded paths and
  iterative child borrowing. The independently leased std workspace is configured
  once; try_lock preserves Send/Sync and prevents overwriting a leased buffer.
  Unconfigured/busy/no_std lookup uses 32 stack slots. Longer paths repeat chunks
  without allocations or treating present nodes as missing. Its header and actual
  capacity join the bridge Temporary ticket before configuration. Unsafe-tree API
  parity adds only zero-storage reporting methods; the production feature stays safe_tree.
  The six small-stack old/new/current probes isolate lookup from build/layout/drop.
  Scope/reparent/removal, held-lease, complexity-bound and real whole-dispatch
  allocation probes cover the changed lookup paths.
  Frozen bounds/clip reject hit candidates before current eligibility lookup.
- paint mode is retained, including when only a different cached callback reruns.
- Local README metadata, minor upstream doc formatting and a mutation callback
  type alias allow strict direct-crate clippy without suppressing warnings.

Complete local source diff is generated under target/reel-artifacts against the
fixed source listed in UPSTREAM-SOURCE.sha256; new files carry their own headers.
`cargo run --locked -p reel-dev -- audit-masonry UPSTREAM` verifies original upstream bytes,
the retained Rust/manifest closure, absence of nested Git and generates the current local diff; no stale generated patch is tracked.
Local README files provide build metadata; they are not imported upstream docs.

R01 adds only the paint metadata/ordering needed by architecture §16:

- VisualLayer groups carry command index, owner WidgetId and forward effect
  space recorded at each paint/append boundary. No owner is inferred from draws.
- Sorted command-owner ranges also capture ordinary Inline children, generated
  clips and debug/inspector painting. Frame export advances a range cursor during
  prepaid command/fragment advancement, including for Work failure diagnostics.
- External retains opaque token, content-local bounds and content transform.
  PaintCtx::set_external_paint declares its retained mode/token. External is
  emitted after pre and own clip, before children; pop clip and post follow.
  Cached callbacks preserve mode/token until explicitly switched to Inline.
- Base and overlay fragments share the layer-local-to-window convention.
  Forward traversal accumulates full compose_local_transform, including layout
  origin and scroll, without inverse matrices. Finite singular transforms survive.
- Debug bounding boxes are already window-space and use identity fragments;
  baselines use forward window transforms. Painter scopes remain continuous
  across these fragments, including with finite singular transforms.

The source manifest, upstream hashes and licenses are unchanged. imaging remains
the pinned ordinary 0.0.1 dependency; no extra source fork or UI ecosystem is added.

Workspace integration keeps masonry/masonry_core 0.4.0, tree_arena 0.2.0 and
linebender_include_doc_path 0.1.0 explicitly. Root workspace dependencies retain
exact versions/features. The fork crates retain the upstream lint table locally:
Reel's root forbid(unsafe_code) must not override tree_arena's test-only unsafe
module or its opt-in unsafe implementation. Production remains std + safe_tree.

Source audit mapping: upstream Cargo.toml -> root Cargo.toml; upstream LICENSE
-> crates/masonry/LICENSE; upstream crate-relative paths -> crates/<path>.
The original 94-file hash manifest is unchanged. Original per-crate licenses and
copyright headers are retained; current new source is presented_geometry.rs and
tree_arena/tests/lookup_depth.rs. The latter replaces the Python lookup probe.

Strict workspace Clippy now checks the selected widgets directly. Button's single
match uses if; Label cache timestamps use checked bounded u8 conversions; existing
Masonry f64 to Parley/Peniko f32 boundaries carry localized documented expectations.
No widget behavior or geometry formula is changed. Two Cargo-wide metadata/feature
name lints are disabled only in fork manifests because they inspect unrelated Reel
packages; the original Rust/API lint and unsafe policies remain intact.
