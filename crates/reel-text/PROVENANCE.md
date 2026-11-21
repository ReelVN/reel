# Reel text provenance

[简体中文](PROVENANCE.zh.md)

The initial implementation came from Parley 0.8.0,
[Linebender/parley](https://github.com/linebender/parley), commit
`8dbecc0545a0c97eb605937b928bc186d2d1295c`, package directory `parley`.
The original Apache-2.0/MIT licenses and source copyright notices are retained.

Reel maintains this implementation as `reel-text`. Local changes include cached
ICU word/line segmentation, public ICU bidi classes, shared HarfRust font
instances and shaping buffers, baseline-aware inline boxes, style-run line
metrics, logical writing axes with Unicode vertical orientation and HarfRust
vertical shaping, and editor wrapping/alignment/overflow geometry. UI paragraph markup
and controls compose these layouts in `reel-ui`.
