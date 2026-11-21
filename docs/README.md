# Reel Engine Developer Documentation

This directory holds **engine-internal documentation** for contributors working on Reel’s runtime, renderer, Aside compiler/VM, and Rust host SDK.

> [!NOTE]
> **Looking to build a game with Reel?**
> Creator tutorials, Aside syntax guides, `resources.toml` recipes, and the searchable API reference have moved to **[reelvn.com](https://reelvn.com)** ([简体中文](https://reelvn.com/zh/docs/quickstart) · [English](https://reelvn.com/en/docs/quickstart) · [`llms.txt`](https://reelvn.com/llms.txt)).
> Editor hover docs, parameter hints, and Go-to-Definition are generated directly from the English `///` doc comments next to Rust declarations.

## Core Specifications

| Document | Scope |
| :--- | :--- |
| [Engine Architecture](engine-architecture.md) | Crate responsibilities, rendering (WGPU / GXM), scheduling, memory budgets, ownership, input, presentation, and media |
| [Aside Language & VM Spec](aside-language.md) | Syntax, evaluation order, gradual types, collections, bytecode, VM execution budgets, GC, and snapshots |
| [Extending Aside from Rust](aside-host.md) | `#[native]` / `Properties` macros, value conversions, bound receivers, and LSP declaration generation |
| [Visual Resource Prediction](visual-resource-prediction.md) | Compile-time image/portrait prefetch planning, memory budgets, and fast-forward cancellation |
| [Performance Verification](next-release-performance.md) | Reproducible desktop/device frame-time profiling and budget validation workflow |

## Examples & Tooling

- Runnable Aside example projects live under [`examples/`](../examples/) (shared sample assets in [`examples/assets/`](../examples/assets/)). Run `reel check` or `reel dev` inside any example directory.
- Development checks, profiling sessions (`reel-dev debug`), and release verification (`reel-dev perf verify`) are documented in [`crates/reel-dev/README.md`](../crates/reel-dev/README.md).
- Contribution and repository hygiene rules live in [`AGENTS.md`](../AGENTS.md).
