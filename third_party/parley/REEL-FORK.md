# Reel's Parley modification

Imported on 2026-10-13 from the published `parley` 0.8.0 crate.
Upstream: https://github.com/linebender/parley
Source commit: `8dbecc0545a0c97eb605937b928bc186d2d1295c`, package directory `parley`.
The original Apache-2.0 and MIT licenses and source copyright notices are retained.

Reel needs Chinese/Japanese word segmentation and complex-script line breaking.
The upstream 0.8.0 analysis code selects ICU's `new_for_non_complex_scripts`
constructors unconditionally, so adding dependency features alone cannot supply
the missing segmentation models.

Local changes:

- Enable ICU Segmenter's `auto` feature alongside its compiled data.
- Construct automatic word segmentation and all three line-breaking modes once
  per `LayoutContext`, then reuse their borrowed data. This loads the CJK word
  dictionary and supported complex-script models while retaining Parley's
  existing `Normal`, `BreakAll`, and `KeepAll` policies.
- Build bidi membership masks from the public `BidiClass` constants instead of
  the deprecated ICU4C-number accessor. ICU's current numeric conversions are
  not const, so this keeps the existing compile-time masks without depending on
  ICU's private numeric representation or adding a dependency.
- Import library source and package/license documents only. The upstream
  snapshot-test module, fixtures, tools, and generated artifacts are not imported;
  its test-module declaration is omitted from `src/lib.rs`. Reel validates this
  change through its real author-scene rendering contract.

The workspace still pins version 0.8.0 and selects this source with
`[patch.crates-io]`. There is no source-hash gate or upstream synchronization
requirement.
