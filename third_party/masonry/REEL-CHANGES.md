# Reel production Masonry closure (U02 / #11)

Upstream: https://github.com/linebender/xilem/tree/271a27a6d4a930f7878d404f9014e3c50a3a9b88
License: Apache-2.0; original LICENSE and file copyright headers retained.
`UPSTREAM-SOURCE.sha256` records original bytes before local changes.

Imported: masonry_core source/docs, tree_arena, include_doc_path procedural macro;
Masonry Button/Image/Slider and Button's Label dependency, their five property
modules, selected default theme and referenced screenshots. No winit, GPU,
Xilem, virtual list, examples or nested Git repository. Unrelated widget modules
and screenshot harness tests are omitted; real-control regression tests live in
reel-stage. No Slider behavior/configuration API is changed by U02.

Local manifest changes: isolated four-crate workspace, selected modules/theme,
no backend/testing features; exact ordinary dependency versions and root lock.
Production geometry/context/input changes will be recorded here as implemented.
