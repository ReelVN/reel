# Borrowed UI source

Reel maintains this code independently. It was borrowed from
[Xilem/Masonry commit 271a27a6](https://github.com/linebender/xilem/tree/271a27a6d4a930f7878d404f9014e3c50a3a9b88):
the widget engine, safe tree, Button/Image/Label/Slider, associated properties,
theme, conceptual documentation and three screenshots. Original Xilem/Druid
copyright and SPDX headers remain in those sources. The three source packages
had identical Apache-2.0 licenses, retained verbatim in [LICENSE](LICENSE);
no NOTICE file was present in the imported closure.

Reel changes include frozen presentation geometry/input contexts, terminal input
cleanup, retained paint modes, bounded safe-tree lookup, portable frame export,
presentation acceptance and budgeted resource leases. Issue #34 consolidates
these responsibilities in one package, merges properties, retires the unused
unsafe tree and documentation macro, and replaces upstream layout/version/patch
replay requirements with local ownership and legal-attribution checks.

Upstream synchronization and contribution are not maintenance goals. Historical
source manifests, diffs and experimental observations remain available at the
[accepted pre-integration base](https://github.com/jhq223/reel/tree/7990c326f1acf3531a940134eab38bd55c66f214/crates/masonry)
and [U01 experiment](https://github.com/jhq223/reel/tree/7990c326f1acf3531a940134eab38bd55c66f214/tools/masonry-contract-check).
Production regression assertions are maintained in `tests/`.

Author-stage controls reuse the maintained Button activation behavior, extracted
as `ButtonBehavior` for shared pointer, focus, keyboard and accessibility actions.
The common stage node model also carries inherited enabled state.

The first-party single-line TextInput widget uses the maintained Parley PlainEditor
for shaping, selections and IME composition. Native IME and clipboard adapters live
in reel-player; reel-ui retains no native window or clipboard dependencies.

Slider range/stepping and thumb geometry are shared with author controls. Interaction
configuration revisions preserve value-update gestures and cancel obsolete captures;
source and ASB control flows are covered by `reel-cli/tests/author_controls.rs`.

Reel's Label paragraph composer adds ranged markup styles, baseline-aligned ruby,
emphasis dots and owned inline-link actions. Text reveal shares the complete layout;
interactive-region changes update focus and presented input configuration.

Image painting supports shared-source repetition, optional nine-slice centers and
node-relative corner tint. Portable frame metadata retains the tint across cached
painting and transforms without platform handles or copied image pixels.
