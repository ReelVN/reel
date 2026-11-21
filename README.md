<p align="center">
  <img src="docs/reel.svg" alt="Reel logo" width="72" height="72">
</p>

# Reel

English · [简体中文](README.zh.md)

Reel is a cross-platform engine for visual novels and 2D narrative games. Write dialogue, stage direction, menus, and embedded 2D games in Aside, the engine’s scripting language.

[Creator guide](https://reelvn.com/en/docs/quickstart) · [API reference](https://reelvn.com/en/reference) · [Playground](https://reelvn.com/playground?lang=en) · [Downloads](https://reelvn.com/en/download)

- Compose custom dialogue, UI, animation, and effects on the same stage.
- Use vertical text, ruby annotations, and inline translations.
- Save story state at named locations and rebuild the scene when loading.
- Develop on Windows, macOS, or Linux. Package desktop releases or a standalone PS Vita game. Try Aside snippets in the browser playground.

## Start a project

Follow the [quickstart](https://reelvn.com/en/docs/quickstart) to create a project and run it with `reel dev`. The [release guide](https://reelvn.com/en/docs/resources-export) covers assets and platform packaging.

## Build from source

From the repository root, use the Rust toolchain pinned in [rust-toolchain.toml](rust-toolchain.toml).

```sh
cargo build --locked --release -p reel-cli --features window
```

This builds `reel`, the `reel-lsp` language server, and the desktop player `reel-game`. Cargo writes them to the configured release output directory.

## Work on the engine

- [Engine documentation](docs/README.md) covers architecture, Aside, and the Rust host interface.
- [Developer tools](crates/reel-dev/README.md) cover repository checks, debugging, and performance measurements.
- [Browser preview](crates/reel-web/README.md) covers building and embedding the WebGPU player.
- [Contribution rules](AGENTS.md) and the [changelog](CHANGELOG.md) record how the project is developed and what changed.

License declarations are in the crate manifests. [Reel UI](crates/reel-ui/LICENSE) uses Apache 2.0. Borrowed code and test fixtures carry their notices alongside the source.
