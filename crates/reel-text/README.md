# Reel text

`reel-text` provides text shaping, Unicode analysis, line breaking, alignment and
editing for Reel. It produces reusable layouts and positioned glyph runs;
`reel-ui` owns widgets and the renderer consumes those glyph runs.

Share a `FontContext` and `LayoutContext` across layouts. Build styled text with
`ranged_builder`, `tree_builder` or `style_run_builder`, then call
`Layout::break_all_lines` and `Layout::align`. A layout can be wrapped again at a
new width without rebuilding its text. Changing text or styles requires a new
layout. The crate documentation includes a complete example using `reel_text`.

The default `system` feature enables system font discovery and `std`. Enable
`accesskit` for accessibility integration. For environments without `std`,
disable default features and enable `libm`; font data must be supplied by the
caller. ICU's cached automatic segmentation supports Chinese/Japanese words and
complex-script line breaking. The existing normal, break-all and keep-all modes
remain available.

The crate follows the workspace Rust version and forbids unsafe code. Its source
and license history are described in [PROVENANCE.md](PROVENANCE.md). It is licensed
under [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT).
