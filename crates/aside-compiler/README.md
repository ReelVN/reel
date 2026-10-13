# Aside frontend, semantic analysis and lowering

Design 1.1 / Issue #8. One Logos scanner and one recursive descent/Pratt parser
produce an `aside-syntax` lossless tree. Includes tuple/group, object/computed keys/spread,
arrays, calls/named arguments/children, closures, all defined operators,
functions/classes/imports, patterns, control flow, exceptions and interpolation.
L03 adds source-owned typed HIR, lexical/module binding, initialization-aware
captures, conservative type/effect checks and injected module resolution.
L04 adds bounded lowering from AnalyzedModule through register IR to the shared
B01 VerifiedProgram; see [LOWERING.md](LOWERING.md) for supported constructs,
limits and explicit rejections. B01's bytecode format and verifier live in
aside-vm. No bytecode executor is included in this compiler. Syntax success does
not imply analysis success; verified bytecode does not establish execution or
full-language support.

L05 / Issue #37 compiles module initializer-local block/condition/loop/catch
bindings and transitive closure/default captures through the existing B01 Cell
instructions. Root globals and exports retain their identities. BindingOrigin
separates lexical ownership from capture storage; NoWait-only ownerless locals,
non-root exports and enclosing bindings without an actual external Value/Cell
link are located rejections. See the [L05 contracts](LOWERING.md#initializer-local-bindings-l05)
and [static measurements](LOWERING_PERFORMANCE.md#2026-10-09-initializer-local-compilation-l05).
These checks do not execute Aside or establish runtime Cell/GC behavior.

L06 / Issue #40 adds fixed tuple/array/table literals and nonoptional index reads,
mixed Name/Index assignment chains and index compound writes using B01's existing
descriptors and instructions. Table keys/values are evaluated before one ordered
MakeTable; assignment receivers/keys are saved across RHS calls. See the
[L06 static contract and matrix](LOWERING.md#fixed-collections-and-index-locations-l06).
Collection spread and patterns remain deferred; L08 adds optional value chains
and L09 adds call spread/rest. No Aside
execution, hashing/index bounds, alias/GC or engine acceptance is claimed.

L07 / Issue #42 adds nonoptional MemberGet/MemberSet, mixed Name/Index/Member
assignment chains and all ten compound assignments. MemberGet results feed the
existing dynamic Call without compiler-injected self. Prepared locations retain
the object value and SymbolId, not a resolved slot or setter. Trusted native
namespace links keep their original identity and contract checks. See the
[L07 static contract](LOWERING.md#member-values-and-locations-l07). Classes/super,
native function values and complete unified callable support remain deferred;
actual receiver binding, hooks and GC behavior are not execution-validated.

L08 / Issue #44 compiles optional Member/Index/Call value chains through existing
BranchNull instructions. Only explicit optional segments guard; ordinary suffixes
share the preceding short-circuit exit. Group ends propagation. Same-chain
assignment targets and optional native values/calls remain located rejections.
See [L08 contracts](LOWERING.md#optional-value-chains-l08). These are static
compilation checks, not runtime side-effect, hook, GC or restoration acceptance.

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

Default budgets: 1,000,000 tokens, 128 recursive/syntax levels and 128 diagnostics.
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
cargo fmt --all --check
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

[tests/guide.rs](tests/guide.rs) maps all 25 scenarios to their actual module or
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
All 25 bilingual scenes are analyzed with actual module/story contexts, explicit
native symbols, and source enclosing bindings (including capture of story `view`).
Reviewed Quirrel migrations retain original assertion tokens, source maps, licenses and output:

```sh
cargo run --locked -p reel-dev -- check
cargo test -p aside-compiler --test semantic_quirrel -- --nocapture
```

`quirrel_vm.rs` executes all 24 migrated samples through B01 and the real VM, both normally and with tiny slices interleaved with GC. It checks the original stdout, while `semantic_quirrel.rs` verifies all 52 original assertion call sites. The complete original test corpus remains available for further migration.
[SEMANTIC_PERFORMANCE.md](SEMANTIC_PERFORMANCE.md) records large-module timing,
allocation/retained storage, named-signature scaling and bounded diagnostics.

## Source execution

The earlier L04–L09 reports above describe their original static acceptance.
Current `tests/execution.rs` compiles source, round-trips B01, initializes a module
and runs its entry on the real V03 VM, including tiny slices with interleaved GC.
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

Full-language coverage and T01 fixture execution remain separate from this matrix.

## Call spread/rest (L09)

Issue #46 / PR #47 adds source-order ArgsNew/Push/Named/Spread followed by a
single Packed Call with no CallDesc names. Calls without spread retain Window.
Script rest occupies Signature.rest and its proper Parameter slot, preserving
Cell/default-prefix/capture identity. Rest receives only surplus positional
arguments as a fresh outer array; its annotation constrains that array, not each
element. A named argument cannot supply rest. Known invalid spread sources and
incompatible rest annotations diagnose; unknown sources retain runtime checks.
See [the contract](LOWERING.md#call-spread-and-rest-l09). Collection spread and
native function values remain separate. The original L09 source/IR/B01 checks
are complemented by the current source execution matrix above.
