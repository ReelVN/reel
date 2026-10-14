# Reel text provenance

The initial text layout implementation came from Parley 0.8.0,
[Linebender/parley](https://github.com/linebender/parley), commit
`8dbecc0545a0c97eb605937b928bc186d2d1295c`, package directory `parley`.
The original Apache-2.0 and MIT licenses and source copyright notices are retained.

Reel maintains this code as the `reel-text` workspace crate. It owns shaping,
Unicode analysis, line breaking, text layout and editing; UI widgets remain in
`reel-ui`. Chinese/Japanese word segmentation and complex-script line breaking
use cached ICU automatic segmenters. Bidi classification uses public ICU classes.
Font discovery, shaping engines, Unicode data and font parsing remain ordinary
pinned dependencies. This package has no upstream synchronization or patch workflow.

Inline boxes support an explicit baseline with ascent/descent accounting for ruby;
omitting it retains bottom-to-baseline alignment. Reel's markup and paragraph
composition live in reel-core and reel-ui, respectively.

Shaping uses HarfRust's shared font instances and reusable in-place buffer;
cached instances share Fontique's font bytes without copying them.
