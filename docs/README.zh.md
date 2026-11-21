# Reel 引擎文档

[English](README.md) · 简体中文

官网提供[创作指南](https://reelvn.com/zh/docs/quickstart)、[API 速查](https://reelvn.com/zh/reference)和[游乐场](https://reelvn.com/playground?lang=zh)。仓库的 `examples/` 保留引擎验证使用的项目、素材和可独立运行的 `author_arcade` 示例。

| 文档 | 内容 |
| --- | --- |
| [引擎架构](engine-architecture.zh.md) | 模块职责、执行、所有权、输入、呈现和媒体 |
| [Aside 语言与 VM 规范](aside-language.zh.md) | 语法、求值、类型、字节码和恢复 |
| [Rust 宿主接口](aside-host.zh.md) | 函数、属性、类型注册和编辑器文档 |
| [Aside 字节码格式](../crates/aside-vm/src/bytecode/FORMAT.zh.md) | 指令编码、链接和运行时校验 |
| [Reel 发行包格式](../crates/reel-package/FORMAT.zh.md) | ASB 节、资源索引和加载 |
| [开发工具](../crates/reel-dev/README.zh.md) | 仓库检查、调试和性能采集 |
| [浏览器预览](../crates/reel-web/README.zh.md) | WebGPU 预览构建和网页嵌入接口 |

编辑器的悬停和参数提示使用 Rust 声明旁的英文文档。开发约定见 [Reel 开发规则](../AGENTS.zh.md)。
