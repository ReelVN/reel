# Aside syntax trees

`aside-syntax` owns the lossless syntax tree used by `aside-compiler`. Immutable
green nodes share source tokens and subtrees; typed syntax nodes and cursors add
language kinds, parent navigation, and byte ranges. The library is independent of
Reel, its VM, and platform libraries.

`GreenNodeBuilder` builds trees, `Language` supplies syntax kinds, and
`SyntaxNode`, `SyntaxToken`, and their iterators expose the resulting structure.
Tree clones retain shared identity. Final green-node release uses an explicit
work list and an atomic final-owner decision, including concurrent clone release
and trees retained solely by a syntax cursor. Long flat expressions therefore do
not consume one native stack frame per node during destruction.

Unsafe implementation code is confined to the reference-counted allocation,
green-node representation, and cursor modules. Compiler and Reel callers use
safe APIs; this package does not claim `unsafe_code = "forbid"` internally.

Ownership and long-expression behavior are exercised through the compiler's
frontend and source execution suites:

```sh
cargo test --locked -p aside-compiler
```

Original licenses, source attribution, and the ownership change are recorded in
[PROVENANCE.md](PROVENANCE.md). Reel maintains this workspace crate directly.
