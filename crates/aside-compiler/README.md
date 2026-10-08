# Aside source frontend

Design 1.1 / Issue #8. One Logos scanner and one recursive descent/Pratt parser
produce a rowan lossless tree. Includes tuple/group, object/computed keys/spread,
arrays, calls/named arguments/children, closures, all defined operators,
functions/classes/imports, patterns, control flow, exceptions and interpolation.
No lowering, binding/type checker, bytecode format or executor is implemented.
Syntax success does not imply name/type/effect validity or a runnable game.

The workspace comes from D01 `c03226a`; only this directory and compiler lockfile
entries change relative to that base. [INTERFACE.md](INTERFACE.md) records the
approved public interface and the owner decisions resolving the original syntax
gaps. Pattern OR binding consistency, names, effects and runtime type rules remain
work for subsequent passes.

```rust
use aside_compiler::{parse, lex, EntryPoint, ParseOptions, parse_with_options};
let source = "let 中文 = (1, 2); // preserved\n";
let parsed = parse(source)?;
assert!(parsed.is_ok());
assert_eq!(parsed.syntax().text().to_string(), source);
let tokens = lex(source)?;
let expression = parse_with_options("|| ({name: \"Mio\"})", ParseOptions {
    entry: EntryPoint::Expression, ..Default::default()
})?;
# Ok::<(), aside_compiler::SourceTooLarge>(())
```

`EntryPoint::Block` parses a function-body fragment (no artificial wrapper or
shifted ranges). Module entry requires ordinary expression statements to have
semicolons; blocks may return a final expression. `Parse::green()/syntax()`
expose reusable immutable and navigable trees. `Diagnostic` and `to_codespan`
provide structured and human output. `SourceMap` gives zero-based line and
byte/scalar/UTF-16 columns without rounding invalid UTF-8 boundaries. Callers
attach file/module-version identity to these source-local results.

All leaves include source text and half-open byte ranges; whitespace, comments
and malformed bytes in valid UTF-8 source are preserved. Missing tokens produce
empty-range diagnostics/fixes instead of invented source text. Error recovery
retains delimiters and continues through following declarations. Logos scans
ordinary tokens; iterative scanner modes handle nested comments/strings/holes.
Identifiers are checked with the pinned unicode-ident XID tables (exported
`UNICODE_VERSION`, currently 18.0.0). Identifier scanning uses that table
for ASCII continuations and non-ASCII scalars, avoiding mismatched regex-table
versions; no normalization changes the spelling.

Default budgets: 8 MiB source, 1,000,000 tokens, 128 recursive/syntax levels,
128 diagnostics. Ordinary token emission is capped exactly; one separate error-tail
leaf may exceed that count to preserve unprocessed text. Source above admission limits returns `SourceTooLarge` before
allocation. Other exhaustion marks `exhausted()`, emits `ASR001`, and preserves
the remaining source as an error subtree. The configured depth is clamped to
1..128 for Rust stack safety; unwind and the lossless error tail may add enclosing
nodes after exhaustion. The parser checks tree height as well as call depth, so
flat postfix/binary/union chains cannot bypass nesting limits. O(source + tokens)
space is required by lossless ownership; exhaustion is not constant-space input
streaming. A truncated UTF-8 byte stream must first be decoded by the caller;
this API accepts valid `&str`, including every valid-boundary source prefix.

Run (Rust 1.96.0):

```sh
cargo fmt --all --check
cargo clippy -p aside-compiler --all-targets --locked -- -D warnings
cargo test -p aside-compiler --locked
cargo test -p aside-compiler --release --locked
cargo run -p aside-compiler --example check -- path/to/file.aside module
cargo build -p aside-compiler --example frontend_probe --release --locked
python3 crates/aside-compiler/bench-results/run_probe.py parse
python3 crates/aside-compiler/bench-results/run_probe.py lex
python3 crates/aside-compiler/bench-results/run_probe.py named
```

`ASIDE_DUMP_TREE=1` prints the actual CST in the check example. Source diagnostics
cause exit status 1; source-admission/IO errors return failure.

[tests/guide.rs](tests/guide.rs) maps all 24 scenarios to their actual module or
story-body contexts, extracts both live guides with line numbers, checks exact
bilingual source equality, and parses both editions. No guide edits or fixture
rewrites conceal discrepancies. [tests/frontend.rs](tests/frontend.rs) checks
tree nesting/ambiguity pairs, malformed literals, Chinese/emoji ranges, recovery,
every UTF-8 truncation of representative forms, seeded arbitrary input at every
entry, and deep/flat budget cases. Else-if chains are additionally tested in an isolated
child process, protecting the test runner from a stack-overflow regression.
The 100,000-name argument case preserves the first duplicate range using a
HashMap rather than repeated linear search. Performance evidence is in
[PERFORMANCE.md](PERFORMANCE.md).
