# Reel

Reel 是面向视觉小说的跨平台游戏引擎，Aside 是其 Rust 实现的嵌入式语言。

设计基线 1.1 已完成架构、语言与作者接口交叉审查，按公共协议、VM、舞台和媒体拆分实现。架构、语言和作者指南见 [docs](docs/README.md)。所有能力状态以文档标记和实际实现为准。

Reel is a cross-platform visual novel engine with Aside, an embedded language implemented in Rust.

Design baseline 1.1 has passed architecture, language, and creator-interface review. Implementation is organized around shared contracts, the VM, the stage, and media. See [documentation](docs/README.md) for the architecture, language specification, and bilingual creator guide.

开发检查与第一方脚本覆盖迁移见 [reel-dev](tools/reel-dev/README.md)。UI 借用源码由 Reel 自主维护在单一 [reel-ui](crates/reel-ui/README.md) 中，按职责使用内部模块，共用根 workspace 和 lockfile；保留原始许可和必要版权；可再生成输出统一放入忽略的 `target/reel-artifacts/`。
