# Reel engine documentation

English · [简体中文](README.zh.md)

The website hosts the [creator guide](https://reelvn.com/en/docs/quickstart), [API reference](https://reelvn.com/en/reference), and [playground](https://reelvn.com/playground?lang=en). The repository's `examples/` contains engine verification projects, fixtures, and the standalone `author_arcade` example.

| Document | Contents |
| --- | --- |
| [Engine architecture](engine-architecture.md) | Module boundaries, execution, ownership, input, presentation, and media |
| [Aside language and VM](aside-language.md) | Syntax, evaluation, types, bytecode, and restoration |
| [Rust host interface](aside-host.md) | Function, property, and type registration and editor documentation |
| [Aside bytecode format](../crates/aside-vm/src/bytecode/FORMAT.md) | Instruction encoding, linkage, and runtime validation |
| [Reel package format](../crates/reel-package/FORMAT.md) | ASB sections, resource indices, and loading |
| [Developer tools](../crates/reel-dev/README.md) | Repository checks, debugging, and performance capture |
| [Browser preview](../crates/reel-web/README.md) | Building and embedding the WebGPU player |

Editor hover and parameter help use the English documentation beside Rust declarations. See the [repository rules](../AGENTS.md) for contributions.
