# UI 源码来源

[English](ORIGIN.md)

控件引擎、安全树、Button、Image、Label、Slider 的初始实现、属性、主题、概念文档及三张截图均来自 Xilem/Masonry 的提交 `271a27a6d4a930f7878d404f9014e3c50a3a9b88`。[上游提交](https://github.com/linebender/xilem/tree/271a27a6d4a930f7878d404f9014e3c50a3a9b88)

源码中保留原有的 Xilem/Druid 版权和 SPDX 文件头。引入的各包共用 Apache-2.0 许可证，其源码快照内没有 NOTICE 文件。[许可证](LICENSE)

Reel 将代码统一维护在工作区包 `reel-ui` 中。本地改动包括冻结显示与输入几何、可移植帧导出、按预算管理的资源、创作者舞台控件、保留式绘制、Popup 层、原生 Tab 导航和嵌套滚动视口的子元素变换。Label 宽度变化时，复用有界布局缓存中已塑形的段落重新换行。字体、样式或文本变化时会使缓存失效。文本塑形与编辑由共用的 `reel-text` 包提供，原生适配器分布在 `reel-player` 和 `reel-render` 中。

控件存储使用 Linebender 单独发布的 `tree_arena` 0.2.0 及其中的平铺存储实现。Reel 的控件树留在 UI 线程，可移植帧与显示几何持有各自使用的共享数据。
