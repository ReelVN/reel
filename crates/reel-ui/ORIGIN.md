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
