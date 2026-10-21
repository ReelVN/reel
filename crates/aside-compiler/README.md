# Aside compiler

One Logos scanner and recursive descent/Pratt parser produce a lossless
`aside-syntax` tree. Semantic analysis produces source-owned typed HIR with
lexical/module binding, initialization-aware captures and type/effect checks.
Lowering turns `AnalyzedModule` into register IR and the shared verified bytecode.
The bytecode model, codec, verifier and executor live in `aside-vm`; the compiler
remains independent of Reel and platform libraries.

Current execution covers ordinary functions and closures, defaults, exceptions,
collection literals and spreads, index/member writes, optional value chains,
class construction/inheritance/static members, script module imports and linking,
host waits, fixed tuple destructuring, and executable children composition. Named table spreads use the same
argument capture path as positional spreads. Remaining array/object/rest and match structural patterns, native
async/generator execution, delegates/hooks and property-bag tails remain open.
See [lowering](LOWERING.md), [semantics](SEMANTICS.md) and the
[language specification](../../docs/aside-language.md) for precise boundaries.

[INTERFACE.md](INTERFACE.md) describes lossless syntax and diagnostics.
Historical compiler increments and their original acceptance are preserved in the
[fixed earlier README](https://github.com/jhq223/reel/blob/f916ffedceb91465633fd549e4db1084c070edf3/crates/aside-compiler/README.md).

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

Default source and token admission use the u32 format capacity; recursive/syntax
levels and diagnostics each default to 128.
Source length defaults to the u32 byte-offset format capacity, with no smaller
8 MiB author limit; hosts may explicitly set a source admission budget. Ordinary token emission is capped exactly; one separate error-tail
leaf may exceed that count to preserve unprocessed text. Source above admission limits returns `SourceTooLarge` before
allocation. Other exhaustion marks `exhausted()`, emits `ASR001`, and preserves
the remaining source as an error subtree. Recursive and structural depth default
to 128; hosts can change these budgets without an implicit maximum clamp.
Unwind and the lossless error tail may add enclosing nodes after exhaustion.
Flat binary and postfix chains use iterative processing and do not consume one
nesting level per operation. Parentheses, blocks, and nested operands retain
their depth accounting. O(source + tokens)
space is required by lossless ownership; exhaustion is not constant-space input
streaming. A truncated UTF-8 byte stream must first be decoded by the caller;
this API accepts valid `&str`, including every valid-boundary source prefix.

Run (Rust 1.96.0):

```sh
cargo fmt -p aside-compiler --check
cargo clippy -p aside-compiler --all-targets --locked -- -D warnings
cargo test -p aside-compiler --locked
cargo test -p aside-compiler --release --locked
cargo run -p aside-compiler --example check -- path/to/file.aside module
cargo build -p aside-compiler --example frontend_probe --release --locked
cargo run --locked -p reel-dev -- compiler-probe frontend parse
cargo run --locked -p reel-dev -- compiler-probe frontend lex
cargo run --locked -p reel-dev -- compiler-probe frontend named
```

`ASIDE_DUMP_TREE=1` prints the actual CST in the check example. Source diagnostics
cause exit status 1; source-admission/IO errors return failure.

[tests/guide.rs](tests/guide.rs) uses the named scene registry in
[guide_scenes.rs](tests/support/guide_scenes.rs) to select each actual module or
story-body context. It extracts both live guides with line numbers, checks exact
bilingual Aside source equality, and parses both editions. [tests/frontend.rs](tests/frontend.rs) checks
tree nesting/ambiguity pairs, malformed literals, Chinese/emoji ranges, recovery,
every UTF-8 truncation of representative forms, seeded arbitrary input at every
entry, and deep/flat budget cases. Else-if chains are additionally tested in an isolated
child process, protecting the test runner from a stack-overflow regression.
The 100,000-name argument case preserves the first duplicate range using a
HashMap rather than repeated linear search. Performance evidence is in
[the fixed frontend measurements](https://github.com/jhq223/reel/blob/f916ffedceb91465633fd549e4db1084c070edf3/crates/aside-compiler/PERFORMANCE.md).

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
The same named bilingual scenes are analyzed with actual module/story contexts, explicit
native symbols, and source enclosing bindings (including capture of story `view`).
Reviewed Quirrel migrations retain original assertion tokens, source maps, licenses and output:

```sh
cargo run --locked -p reel-dev -- check
cargo test -p aside-compiler --test semantic_quirrel -- --nocapture
```

`quirrel_vm.rs` executes every migration registered in
[the fixture catalog](tests/fixtures/quirrel/README.md) through bytecode and the
real VM, normally and with tiny slices interleaved with GC. It checks original
stdout; `semantic_quirrel.rs` verifies the retained assertion call sites.
The complete original corpus remains available for further migration.
[The fixed semantic measurements](https://github.com/jhq223/reel/blob/f916ffedceb91465633fd549e4db1084c070edf3/crates/aside-compiler/SEMANTIC_PERFORMANCE.md) records large-module timing,
allocation/retained storage, named-signature scaling and bounded diagnostics.

## Source execution

`tests/execution.rs` compiles source, round-trips bytecode, initializes a module
and runs its entry on the real VM, including tiny slices with interleaved GC.
It checks arithmetic, shared captures, defaults, finally, prepared index targets,
members, optional short-circuiting, spread/rest and loops against scalar results.
Trailing children now executes with independent nested collectors, ordinary helper
functions, and array/tuple `for` loops. Each iteration has its own captured binding.
Only host values registered for composition are collected; null is ignored,
declarations and assignments do not contribute children. Shared host fields retain
identity through collection, calls and later property writes.

```sh
cargo test --locked -p aside-compiler --test execution
cargo test --locked -p aside-compiler --test execution --release
```

Class and module execution have dedicated `classes` and `lowering_modules` tests.
The remaining Quirrel corpus is tracked separately from this execution matrix.

## Call spread/rest

Expanded calls emit source-order ArgsNew/Push/Named/Spread followed by a
single Packed Call with no CallDesc names. Calls without spread retain Window.
Script rest occupies Signature.rest and its proper Parameter slot, preserving
Cell/default-prefix/capture identity. Rest receives only surplus positional
arguments as a fresh outer array; its annotation constrains that array, not each
element. A named argument cannot supply rest. Known invalid spread sources and
incompatible rest annotations diagnose; unknown sources retain runtime checks.
See [the contract](LOWERING.md#call-spread-and-rest). Collection spread and
native function values have their own checks in the source execution matrix.
