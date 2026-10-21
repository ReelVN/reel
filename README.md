# Reel

Reel 是面向视觉小说的跨平台游戏引擎，Aside 是其 Rust 实现的嵌入式语言。

Reel 支持 Aside 组件、对白与选择、图片、音视频、存档和源码/ASB 桌面运行，也提供资源打包与作品导出。当前能力、平台验证范围和示例见[能力表](docs/README.md#当前状态)，设计与创作接口见[文档](docs/README.md)。

Reel is a cross-platform visual novel engine with Aside, an embedded language implemented in Rust. It supports component-based scenes, dialogue, choices, media, saves, source/ASB playback, resource packaging and project export. See the [capability table](docs/README.md#当前状态) for implementation and platform verification status, and [documentation](docs/README.md) for the architecture, language and bilingual creator guide.

Reel 支持 Aside 组件组合、文字、PNG/JPEG 图片与行列/层叠布局、节点与整组组件的旋转/缩放/锚点、属性更新、对白和选择，以及源码/ASB 窗口运行。基础节点与剧情服务默认可用，无需逐文件导入。可从 [作者舞台示例](examples/author_stage.aside) 开始，在 main 中配置作品的字体文件；下列命令使用的临时资源目录按[中文指南](docs/guide/zh-CN.md)或 [English guide](docs/guide/en.md)准备。

```sh
cargo run --locked -p reel-cli -- check examples/author_stage.aside
cargo run --locked -p reel-cli --features window -- dev examples/author_stage.aside --assets target/reel-artifacts/author-assets
```

开发检查与第一方脚本覆盖迁移见 [reel-dev](crates/reel-dev/README.md)。UI 借用源码由 Reel 自主维护在单一 [reel-ui](crates/reel-ui/README.md) 中，按职责使用内部模块，共用根 workspace 和 lockfile；保留原始许可和必要版权；可再生成输出统一放入忽略的 `target/reel-artifacts/`。
