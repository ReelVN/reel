# Reel

Reel 是面向视觉小说的跨平台游戏引擎，使用 Rust 实现的 Aside 语言创作。作者用普通函数串联剧情，用节点和有状态组件组合画面、交互与演出。

Reel is a cross-platform visual novel engine with Aside, an embedded language implemented in Rust. Authors compose story flow with functions and build scenes with nodes and stateful components.

- [文档 / Documentation](docs/README.md)
- [中文创作指南](docs/guide/zh-CN.md) · [English creator guide](docs/guide/en.md)
- [架构](docs/engine-architecture.md) · [语言规范](docs/aside-language.md)
- [作者舞台示例](examples/author_stage/src/main.aside)

安装工具包并将其 `bin` 加入 PATH 后，在项目目录运行：

```sh
reel check
reel dev
reel build
```

从源码构建工具：`cargo build --locked -p reel-cli --features window`。将 `target/debug` 加入 PATH；项目资源和字体配置见创作指南。

仓库检查工具见 [reel-dev](crates/reel-dev/README.md)。借用实现的许可和来源保存在对应 crate，生成输出放在忽略的 `target/reel-artifacts/`。
