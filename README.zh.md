<p align="center">
  <img src="docs/reel.svg" alt="Reel 标志" width="72" height="72">
</p>

# Reel

[English](README.md) · 简体中文

Reel 是面向视觉小说和 2D 叙事游戏的跨平台引擎。用内置的 Aside 语言编写对白、演出、菜单和嵌入式 2D 游戏。

[创作指南](https://reelvn.com/zh/docs/quickstart) · [API 速查](https://reelvn.com/zh/reference) · [游乐场](https://reelvn.com/playground?lang=zh) · [下载](https://reelvn.com/zh/download)

- 在同一棵舞台树上组合自定义对白、界面、动画和效果。
- 支持竖排文字、振假名注音和内联翻译。
- 将剧情状态存到命名位置，读档时重建场景。
- 在 Windows、macOS 或 Linux 上开发，发行桌面版或独立的 PS Vita 游戏。也可以直接在浏览器游乐场试写 Aside。

## 开始创作

按[快速开始](https://reelvn.com/zh/docs/quickstart)创建项目，再用 `reel dev` 运行。[发行指南](https://reelvn.com/zh/docs/resources-export)介绍资源处理和多平台打包。

## 从源码构建

在仓库根目录使用 [rust-toolchain.toml](rust-toolchain.toml) 锁定的 Rust 工具链构建。

```sh
cargo build --locked --release -p reel-cli --features window
```

这会构建 `reel`、语言服务器 `reel-lsp` 和桌面播放器 `reel-game`。产物写入 Cargo 配置的发行构建目录。

## 开发引擎

- [引擎文档](docs/README.zh.md)介绍架构、Aside 和 Rust 宿主接口。
- [开发工具](crates/reel-dev/README.zh.md)介绍仓库检查、调试和性能测量。
- [浏览器预览](crates/reel-web/README.zh.md)介绍 WebGPU 播放器的构建和嵌入。
- [贡献规则](AGENTS.zh.md)和[更新历史](CHANGELOG.zh.md)记录项目的开发约定和版本改动。

许可声明见各 crate 的清单。[Reel UI](crates/reel-ui/LICENSE)采用 Apache 2.0。借用的代码和测试素材在源码旁保留各自的许可声明。
