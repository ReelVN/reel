# Quirrel-derived execution cases

The original Quirrel test inputs, expected results and runner reference remain in
`original/`, from commit `19f17df9b6ee43c91ef5c635fd45970ec76c5dbe`. See `LICENSE`.
The original suite selected 1,296 inputs and 1,494 runs including iteration seeds.
The C++ implementation is not part of this workspace.

`provenance.json` lists 69 reviewed complete Aside migrations. All 140 original
assertion call sites and original stdout are retained. `semantic_quirrel.rs`
checks assertion tokens and copied source-map spans; `quirrel_vm.rs` compiles,
round-trips B01 and executes each case in the real VM, both normally and with
tiny slices interleaved with GC. It compares stdout to the unchanged `.out`.
Output comparison follows the original runner's line boundaries, accepting the
presence or absence of the final line ending. Interpolation-prefix edits inside
assertions are explicitly mapped and checked as `$"` to `f"`; predicates,
constants, template text and expressions remain unchanged.

```sh
cargo test --locked -p aside-compiler --test semantic_quirrel --test quirrel_vm
```

The remaining inputs are retained for migration, not claimed as passing Aside
execution. Quirrel compiler-specific diagnostics/AST/optimization outputs are
reference oracles, not Aside output contracts. `deliberate-deltas.json` records
language differences; a corresponding native Aside contract must be verified
before a differing behavior is considered covered. No original assertion is
weakened to make a conversion pass. The 10 original recipes remain as provenance;
current reviewed sources and maps are the maintained inputs.

The added closure-loop cases retain all output lines, including nested and
shadowed captures. Runtime type cases retain the original no-error/output
oracle and execute the complete script in an explicit function for top-level
return. Quirrel code-block local returns translate to Aside expression tails
and explicit local result slots; their original exception assertions remain
unchanged. These translations do not implement Quirrel compiler internals or
claim its optimizer/code-block opcodes exist in Aside.
