# Quirrel-derived execution cases

`original/` retains Quirrel inputs, expected results and runner references from
commit `19f17df9b6ee43c91ef5c635fd45970ec76c5dbe`. See [LICENSE](LICENSE).
`provenance.json` catalogs reviewed Aside translations and source maps;
`deliberate-deltas.json` records language differences requiring native contracts.

`semantic_quirrel.rs` checks original assertion tokens and mapped spans.
`quirrel_vm.rs` compiles registered cases, round-trips bytecode and executes them
with normal/tiny slices and interleaved GC, comparing unchanged stdout oracles.
Line comparison uses the original runner's line-ending rules. Interpolation-prefix changes retain explicit source mappings.

```sh
cargo test --locked -p aside-compiler --test semantic_quirrel --test quirrel_vm
```

Closure/loop translations retain nested and shadowed captures. Top-level-return
cases execute in explicit functions; code-block local returns use expression
tails or local result slots while preserving exception assertions. Original
compiler-specific diagnostics/AST/optimization files remain reference inputs;
registered Aside cases carry the maintained execution coverage.
