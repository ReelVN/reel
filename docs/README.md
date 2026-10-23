# Reel 文档

Reel 用代码组合画面、对白、UI、演出和嵌入的 2D 游戏。

- [中文作者指南](guide/zh-CN.md) / [English creator guide](guide/en.md)：项目、组件、剧情、资源与发布。
- [引擎架构](engine-architecture.md)：模块职责、执行、所有权、输入、呈现和媒体。
- [Aside 语言规范](aside-language.md)：语法、求值、类型、执行与恢复。
- [Rust 宿主接口](aside-host.md)：注册函数、属性、类型和编辑器文档。

编辑器中的悬停、参数提示与 F12 使用声明旁的英文 API 文档。

## 示例

| 内容 | 项目 |
| --- | --- |
| 项目组织与基本画面 | [author_project](../examples/author_project/src/main.aside)、[author_stage](../examples/author_stage/src/main.aside) |
| 对白与阅读 | [author_narrative](../examples/author_narrative/src/main.aside)、[author_rich_text](../examples/author_rich_text/src/main.aside) |
| 交互与布局 | [author_buttons](../examples/author_buttons/src/main.aside)、[author_popup](../examples/author_popup/src/main.aside)、[author_list_gallery](../examples/author_list_gallery/src/main.aside) |
| 演出与生命周期 | [author_activities](../examples/author_activities/src/main.aside)、[author_scope](../examples/author_scope/src/main.aside)、[author_transition](../examples/author_transition/src/main.aside) |
| 剧情与玩法 | [author_arcade](../examples/author_arcade/src/main.aside)、[author_embedded_puzzle](../examples/author_embedded_puzzle/src/main.aside) |
| 保存与翻译 | [author_saves](../examples/author_saves/src/main.aside)、[localization](../examples/localization/src/main.aside) |

准备示例引用的字体和素材，在其项目目录运行 `reel check` 与 `reel dev`。共享素材位于 `examples/assets/`。

## 引擎开发

检查命令见 [reel-dev](../crates/reel-dev/README.md)。协作规则见 [AGENTS.md](../AGENTS.md)。借用源码和测试输入的许可证与来源随对应目录保存。
