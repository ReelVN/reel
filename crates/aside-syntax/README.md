# Aside syntax trees

`aside-syntax` owns the lossless syntax tree used by `aside-compiler`.
`GreenNodeBuilder` constructs shared immutable nodes/tokens; `Language` supplies
syntax kinds, and `SyntaxNode`/`SyntaxToken` expose parent navigation and ranges.

Final green-node release uses an explicit work list and an atomic final-owner
decision. This covers concurrent clone release and trees retained by cursors,
allowing long flat expressions to release with bounded native stack use.
Internal unsafe implements reference-counted allocation, node representation and
cursors; the compiler consumes their safe public APIs.

```sh
cargo test --locked -p aside-compiler
```

Source and licenses are recorded in [PROVENANCE.md](PROVENANCE.md).
