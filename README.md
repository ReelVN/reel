# Reel

Reel 是面向视觉小说和 2D 叙事游戏的跨平台引擎。Aside 是用 Rust 实现的嵌入式语言，用于组织剧情、画面、交互和演出。

Reel is a cross-platform engine for visual novels and 2D narrative games. Its embedded Aside language is implemented in Rust and used for story flow, presentation, interaction, and choreography.

- [官网](https://reelvn.com) · [中文创作指南](https://reelvn.com/zh/docs/quickstart) · [English creator guide](https://reelvn.com/en/docs/quickstart) · [API reference](https://reelvn.com/en/reference)
- [引擎文档 / Engine documentation](docs/README.md) · [Changelog](CHANGELOG.md)

## Build from source

From the repository root, build the desktop CLI, language server, and player with the pinned Rust toolchain.

```sh
cargo build --locked -p reel-cli --features window
```

See [Reel developer tools](crates/reel-dev/README.md) for repository checks, diagnostics, and performance verification. See [browser preview](crates/reel-web/README.md) for building and embedding the WebGPU player.

Borrowed implementations and test inputs carry their licenses and provenance alongside the source. Generated reports belong under `target/reel-artifacts/`. [Contribution rules](AGENTS.md)
