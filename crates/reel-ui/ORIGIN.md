# Borrowed UI source

The widget engine, safe tree, initial Button/Image/Label/Slider implementations,
properties, theme, conceptual documentation and three screenshots came from
[Xilem/Masonry commit 271a27a6d4a930f7878d404f9014e3c50a3a9b88](https://github.com/linebender/xilem/tree/271a27a6d4a930f7878d404f9014e3c50a3a9b88).
Original Xilem/Druid copyright and SPDX headers remain in those sources. The
imported packages shared the Apache-2.0 license retained in [LICENSE](LICENSE).
The imported source contained no NOTICE file.

Reel maintains the code in one `reel-ui` workspace package. Local changes include
frozen presentation/input geometry, portable frame export,
budgeted resources, author-stage controls, retained drawing and Popup layers.
Label width changes reuse shaped paragraphs for line breaking within the existing
bounded layout cache; font, style and text changes invalidate that cache.
Text shaping/editing comes from the shared `reel-text` package; native adapters
live in `reel-player` and `reel-render`.

Widget storage uses the separately published `tree_arena` 0.2.0 dependency from
Linebender, with its flat storage implementation. Reel's widget tree stays on
the UI thread; portable frames and presentation geometry own their shared data.
