# Reel text

`reel-text` provides shaping, Unicode analysis, line breaking, alignment and
editing. It produces reusable layouts and positioned glyph runs; `reel-ui`
composes them into widgets and the renderer consumes the glyph runs.

Share `FontContext` and `LayoutContext` across layouts. Build styled text with
`ranged_builder`, `tree_builder` or `style_run_builder`, then break lines and
align. Changing width/wrap/alignment can reuse shaping; changing text/styles
rebuilds the layout. `PlainEditor` owns selections, composition and caret geometry.
The [crate source documentation](src/lib.rs) includes a complete layout example.

The default `system` feature enables system font discovery and `std`.
`accesskit` adds accessibility integration. For `no_std`, select `libm` with
default features disabled and supply font bytes. Cached ICU segmenters support
Chinese/Japanese words and complex-script line breaking.

The crate follows the workspace toolchain and forbids unsafe code. Source and
local changes are recorded in [PROVENANCE.md](PROVENANCE.md), with retained
[Apache-2.0](LICENSE-APACHE) and [MIT](LICENSE-MIT) licenses.
