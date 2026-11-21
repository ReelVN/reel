# Reel 引擎文档

游戏开发教程与 API 参考发布在官网。 [中文创作指南](https://reelvn.com/zh/docs/quickstart) · [中文 API 速查](https://reelvn.com/zh/reference) · [English creator guide](https://reelvn.com/en/docs/quickstart) · [English API reference](https://reelvn.com/en/reference)

教程中的创作示例可以在官网游乐场运行。仓库的 `examples/` 保留引擎验证使用的项目和素材，以及可独立运行的 `author_arcade`。 [官网游乐场](https://reelvn.com/playground)

| 文档 | 内容 |
| --- | --- |
| [引擎架构](engine-architecture.md) | 模块职责、执行、所有权、输入、呈现和媒体 |
| [Aside 语言与 VM 规范](aside-language.md) | 语法、求值、类型、字节码和恢复 |
| [Rust 宿主接口](aside-host.md) | 函数、属性、类型注册和编辑器文档 |
| [开发工具](../crates/reel-dev/README.md) | 仓库检查、调试和性能采集 |
| [浏览器预览](../crates/reel-web/README.md) | WebGPU 预览构建和网页嵌入接口 |

编辑器的悬停和参数提示使用 Rust 声明旁的英文文档。仓库规则见 [AGENTS.md](../AGENTS.md)。
