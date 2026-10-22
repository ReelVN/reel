# Reel 文档

Reel 让创作者用代码组合画面、对白、UI、演出和嵌入的 2D 游戏。先阅读作者指南，在编辑器中通过悬停、补全与定义跳转查看英文 API 文档。

- [中文作者指南](guide/zh-CN.md) / [English creator guide](guide/en.md)：创建项目、组织画面和剧情、交互、资源与发布。
- [引擎架构](engine-architecture.md)：模块职责、所有权、输入、呈现、媒体和资源预算。
- [Aside 语言规范](aside-language.md)：语法、求值、类型、执行、宿主调用与恢复。
- [Rust 宿主扩展](aside-host.md)：声明函数和属性，并接入编译器、VM 与编辑器。
- [分块图片](prepared-images.md)：大图预处理、文件格式和运行时工作集。

## 当前状态

| 领域 | 可用能力 | 后续工作 |
| --- | --- | --- |
| 语言与编辑器 | 模块、函数、类、可观察状态、宿主等待、执行快照；检查、格式化、补全与定义跳转 | [类型推断、语言与 LSP #65](https://github.com/jhq223/reel/issues/65) |
| 画面与交互 | 自定义 ADV/NVL、选择和历史；图片控件、Flex/Grid、虚拟列表、锚定浮层、文本编辑；键鼠、触摸和手柄动作 | [组合表达 #132](https://github.com/jhq223/reel/issues/132) |
| 演出 | 组合动画、曲线、时钟、临时作用域、遮罩、转场、粒子、相机和分块大图 | [效果 #92](https://github.com/jhq223/reel/issues/92) |
| 媒体与资源 | 短音效、流式长音轨、片段队列、视频节点、后台资源加载和分卷发布 | [音频 #67](https://github.com/jhq223/reel/issues/67)、[运行成本 #152](https://github.com/jhq223/reel/issues/152) |
| 项目与保存 | 固定项目入口、源码内翻译、开发与发布导出、执行快照和独立系统数据 | [保存与恢复 #91](https://github.com/jhq223/reel/issues/91) |

当前优先完善类型推断；生成器保留在语言计划中，[2D 场景 #101](https://github.com/jhq223/reel/issues/101) 留到下一轮。各项范围和推进顺序见[总清单 #94](https://github.com/jhq223/reel/issues/94)。设备支持以对应平台的实际验证为准。

## 可运行示例

- [基本画面](../examples/author_stage/src/main.aside)、[组合动画](../examples/author_activities/src/main.aside)和[交互按钮](../examples/author_buttons/src/main.aside)。
- [临时菜单](../examples/author_prompt/src/main.aside)和[下拉选择](../examples/author_popup/src/main.aside)。
- [虚拟图库](../examples/author_list_gallery/src/main.aside)、[多行编辑](../examples/author_text_area/src/main.aside)和[嵌入谜题](../examples/author_embedded_puzzle/src/main.aside)。

进入示例项目后运行 `reel check` 和 `reel dev`；资源准备见示例注释与作者指南。

## 引擎开发

开发命令见 [reel-dev](../crates/reel-dev/README.md) 和各 crate README，协作要求见 [AGENTS.md](../AGENTS.md)。Quirrel 测试输入与迁移记录见 [compiler fixtures](../crates/aside-compiler/tests/fixtures/quirrel/README.md)。
