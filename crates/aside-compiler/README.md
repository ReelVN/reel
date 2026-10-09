# Aside source frontend and semantic analysis

Design 1.1 / Issue #8. One Logos scanner and one recursive descent/Pratt parser
produce a rowan lossless tree. Includes tuple/group, object/computed keys/spread,
arrays, calls/named arguments/children, closures, all defined operators,
functions/classes/imports, patterns, control flow, exceptions and interpolation.
L03 adds source-owned typed HIR, lexical/module binding, initialization-aware
captures, conservative type/effect checks and injected module resolution.
No lowering, bytecode format or executor is implemented. Syntax success does not
imply analysis success, and analysis success does not imply execution.

The workspace comes from D01 `c03226a`; only this directory and compiler lockfile
entries change relative to that base. [INTERFACE.md](INTERFACE.md) records the
approved public interface and the owner decisions resolving the original syntax
gaps. [SEMANTICS.md](SEMANTICS.md) records HIR v1 and the analysis API on accepted
L02 `a50f1f7`, using frozen language `999a8e3`. Only compiler/necessary lockfile
changes are part of L03; author guides remain owned by the integration lead.

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

Semantic source use (host/module policy is explicit):

```rust
use aside_compiler::{analyze, AnalysisOptions, semantic::NoModules};
let parsed = aside_compiler::parse("fn add(a: int, b: int = 1) -> int { a+b } add(2);")?;
let result = analyze(&parsed, &AnalysisOptions::default(), &mut NoModules);
let module = result.into_module().expect("source must have no unresolved diagnostics");
assert_eq!(module.functions().len(), 1);
# Ok::<(), aside_compiler::SourceTooLarge>(())
```

`AnalysisOptions` supplies versioned module identity, real execution context,
explicit host registrations and lexical enclosing bindings for fragments.
`ModuleResolver` accepts logical import requests; `MemoryResolver` and
`analyze_project` support in-memory module fixtures and legal SCC initialization
plans. No compiler filesystem policy or VM dependency is introduced.

Source checks live in `tests/semantics.rs`, `semantic_modules.rs`,
`semantic_guides.rs`, `semantic_quirrel.rs`, and `semantic_depth.rs`.
All 24 bilingual scenes are analyzed with actual module/story contexts, explicit
native symbols, and source enclosing bindings (including capture of story `view`).
Accepted T01 fixture bytes/maps/license are immutable and locally hash-checked:

```sh
python3 crates/aside-compiler/tests/fixtures/quirrel/check_integrity.py
cargo test -p aside-compiler --test semantic_quirrel -- --nocapture
```

Those samples report parse, analysis and `execution=waiting_runtime` separately.
[SEMANTIC_PERFORMANCE.md](SEMANTIC_PERFORMANCE.md) records large-module timing,
allocation/retained storage, named-signature scaling and bounded diagnostics.
