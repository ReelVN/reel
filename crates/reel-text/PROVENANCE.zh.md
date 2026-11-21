# Reel 文本库来源

[English](PROVENANCE.md)

初始实现来自 Parley 0.8.0，提交为 `8dbecc0545a0c97eb605937b928bc186d2d1295c`，包目录为 `parley`。保留原有的 Apache-2.0 和 MIT 许可证及源码版权声明。[Linebender/parley](https://github.com/linebender/parley)

Reel 以 `reel-text` 维护该实现。本地改动包括缓存 ICU 分词和换行分段、公开 ICU 双向文本类别、共享 HarfRust 字体实例和塑形缓冲区、按基线对齐的行内盒、按样式段计算的行度量、支持 Unicode 竖排方向及 HarfRust 竖排塑形的逻辑书写轴，以及编辑器的换行、对齐和溢出几何计算。`reel-ui` 的段落标记和控件在这些布局能力上组合实现。
