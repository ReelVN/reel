# Reel

An open-source visual novel and 2D narrative game engine written in Rust, powered by the embedded **Aside** scripting language.

**[Website & Creator Guide (reelvn.com)](https://reelvn.com)** · **[中文文档](https://reelvn.com/zh/docs/quickstart)** · **[English Guide](https://reelvn.com/en/docs/quickstart)** · **[API Reference](https://reelvn.com/en/reference)** · **[`llms.txt`](https://reelvn.com/llms.txt)** · **[Changelog](CHANGELOG.md)**

> [!IMPORTANT]
> **LLM-Assisted Development Disclosure / 大语言模型协作开发说明**
>
> Large parts of Reel’s implementation, compiler, tooling, and documentation were written in close collaboration with Large Language Models (LLMs) under human architectural direction, code review, and hardware testing.
>
> We disclose this upfront and do not treat LLM assistance as an excuse for lower standards: language semantics, cross-version save restoration, executable API documentation examples, and performance budgets are backed by automated test suites and real-device profiling in this repository.
>
> Reel 的大量源码、编译器实现与文档均在作者主导设计和审查的前提下借助大语言模型（LLM）协作编写完成，所有核心语义、跨版本存档与性能目标均由仓库内的自动化测试与实机数据把关。

---

## What is Reel?

Reel lets you author an entire visual novel or 2D narrative game in one language (**Aside**) instead of stitching together separate script dialects, UI templates, save serializers, and platform shaders:

- **One unified stage tree**: backdrops, layered portraits, dialogue boxes, menus, masks, particles, video, and 2.5D parallax cameras share one coordinate, layout, and lifetime model.
- **Reactive UI in Aside**: `model`, `bind`, and `derive` drive sliders, toggles, virtualized backlog lists, tabbed settings, IME text inputs, and gamepad focus without boilerplate.
- **Book-grade CJK & multi-language typography**: built-in `vertical-rl` / `vertical-lr` vertical text, inline `[ruby=...]` furigana, smooth per-glyph `reveal_fade`, and inline `tr()` localization.
- **Pure-Aside visual effects**: write fragment shaders and dissolve masks as normal typed Aside functions, compiled automatically for Desktop (WGPU) and PlayStation Vita (GXM).
- **Saves that survive script patches**: logical saves (`root`, `flow`, `point`, `state`) store data and named checkpoints rather than fragile VM call stacks, and support direct backlog jumping.
- **Built-in tooling**: `reel` CLI, VS Code LSP (`reel-lsp`), formatter (`reel fmt`), asset pipeline (`resources.toml`), and profiler (`reel-dev`).

---

## Documentation Map

- **Making a game with Reel?** Start at **[reelvn.com](https://reelvn.com)** ([中文创作指南](https://reelvn.com/zh/docs/quickstart) · [English Creator Guide](https://reelvn.com/en/docs/quickstart) · [API Reference](https://reelvn.com/en/reference) · [`llms.txt`](https://reelvn.com/llms.txt)).
- **Working on the engine itself?** Everything under [`docs/`](docs/README.md) is dedicated to engine contributors:
  - [`docs/engine-architecture.md`](docs/engine-architecture.md) — crate boundaries, rendering, scheduling, ownership, audio, and platform backends
  - [`docs/aside-language.md`](docs/aside-language.md) — Aside syntax, evaluation order, type checking, bytecode, and snapshots
  - [`docs/aside-host.md`](docs/aside-host.md) — exporting Rust functions, types, properties, and LSP docs to Aside
  - [`docs/visual-resource-prediction.md`](docs/visual-resource-prediction.md) — compile-time visual asset prefetching
  - [`docs/next-release-performance.md`](docs/next-release-performance.md) — frame-time profiling and release performance verification
  - [`crates/reel-dev/README.md`](crates/reel-dev/README.md) — repository checks, debug sessions, and perf harness

---

## Quick Start

### Option 1: Prebuilt Desktop Toolchain

Download the package for Windows, macOS, or Linux from [GitHub Releases](https://github.com/jhq223/reel/releases) (or [reelvn.com/download](https://reelvn.com/en/download)), add its `bin` folder to `PATH`, and run inside your project directory:

```sh
reel check              # Compile and check syntax, types, and properties
reel dev                # Run from source with incremental caching
reel fmt                # Format .aside source files
reel build --release    # Compile bytecode to build/release/game.asb
reel export --release   # Package a distributable player bundle under dist/
```

### Option 2: Build from Source

Requires Rust `1.98.1+` (edition 2024).

```sh
git clone https://github.com/jhq223/reel.git
cd reel
cargo build --locked -p reel-cli --features window
```

Add `target/debug` (or `target/release` for `--release`) to `PATH`. Check out [`examples/author_stage/src/main.aside`](examples/author_stage/src/main.aside) and the other projects under [`examples/`](examples/) to see working projects.

---

## Workspace Overview

| Crate(s) | Role |
| :--- | :--- |
| `aside-syntax`, `aside-compiler`, `aside-vm` | Independent Aside parser, static analyzer, formatter, bytecode compiler, and VM |
| `aside-host`, `aside-host-macros`, `aside-std`, `aside-lsp` | Rust embedding SDK, standard library, and Language Server Protocol implementation |
| `reel-core`, `reel-engine` | Core budgets/IO contracts, story runtime, stage API, activities, scopes, and logical saves |
| `reel-ui`, `reel-text` | Retained stage tree, Flex/Grid layout, widgets, input, and Unicode/CJK text shaping |
| `reel-render`, `reel-audio`, `reel-media`, `reel-storage` | WGPU & GXM renderers, real-time mixer, audio/video decoding, and atomic file storage |
| `reel-player`, `reel-package`, `reel-cli`, `reel-dev` | Shared player runtime, ASB/RPK packager, user CLI + LSP binary, and engineering verification tools |

`Aside` remains strictly independent of Reel and platform libraries. Production crates enforce `unsafe_code = "forbid"` at the workspace level, keeping native handles isolated in platform adapter modules.

---

## License & Provenance

- First-party workspace crates are dual-licensed under **MIT OR Apache-2.0**.
- Borrowed and locally maintained implementations keep their upstream licenses, `NOTICE` files, and provenance inside their respective crates (`crates/reel-ui` from Masonry under **Apache-2.0**, `crates/reel-text` from Parley, `crates/reel-audio` from Kira, and `crates/aside-syntax` from Rowan).
- Generated diagnostics, profiles, and test reports belong under ignored `target/reel-artifacts/`. See [`AGENTS.md`](AGENTS.md) for development conventions.
