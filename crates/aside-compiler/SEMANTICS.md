# L03 semantic interface v1

Scope: `aside-compiler` HIR, lexical binding, capture, injectable module imports,
control context, and statically provable diagnostics. Baseline L02 `a50f1f7`;
frozen language `999a8e3:docs/aside-language.md` §3, §7–14, §22.
No opcode/runtime, B01 code, platform paths, or author-guide changes.

`Hir` is a source-owned flat arena. Each grammar production has a `HirKind`;
ordered significant tokens carry exact original byte spans (including string
segments/operators). Children and tokens share a packed edge arena; names are
interned once. There is no CST handle in the semantic result. `NodeFacts` maps
reads/writes to stable `SymbolId`, function identity, scopes, known types,
runtime checks and explicit control transfers through finally regions.
`AnalyzedModule` has no public constructor: lowering must require this gate,
which is produced only with a valid parse and no unresolved error diagnostics.
Syntax/analysis success never claims execution.

Algorithm:

1. Admit bounded, successful CST; project each node/token once. Keep syntax-error
   diagnostics and refuse semantic success for recovery/error CSTs.
2. For each lexical block, predeclare read-only fn bindings and pending lets/
   classes. Visit statements in order. Reads of same-frame uninitialized names
   diagnose; deferred function captures retain an initialization runtime check.
   Analyze a function's defaults before introducing that parameter, with earlier
   parameters visible and later parameter names forbidden. Capture references
   through every intervening function using definition-site scope.
3. Mutable and forward read-only captures use a shared symbolic cell. Symbols
   preserve initialization timing, type, mutability and declaration span. Named
   fn environments are linked at scope entry; later declarations initialize the
   same cell once. Read-only cells retain a read-only write contract. Initialized
   immutable expression-closure captures may use values (object identity stays
   shared). Mutual recursion and later captures can promote earlier captures to
   cells across every user. Loop binding scopes carry
   per-iteration allocation, including mutable cells. Class instance methods
   require explicit first self; static methods and ordinary table closures do
   not acquire an implicit receiver. super is lexical to a derived method.
4. Pattern binding collection checks duplicate names and equal OR alternatives.
   Defaults run before atomic introduction of names. Assignment patterns resolve
   existing mutable lexical bindings only; all targets precede the final RHS.
5. Visit control heads, branch scopes, guards, catch and finally in context.
   Record loop/function targets and finally cleanup chains. A finally-local loop
   may break locally; return or jumping beyond the finally boundary diagnoses.
6. Infer conservative facts. Definite strict-bool, annotation, signature, await
   and effect conflicts diagnose. Unknown dynamic types/call effects preserve
   runtime checks. Resolve known function wait effects over call edges using a
   worklist (not repeated whole-tree traversal). Async/gen creation does not
   execute bodies. `known_wait` is a possibility summary and never rejects a
   NoWait call. Such calls preserve `NO_WAIT` runtime checks; conditional paths
   also preserve runtime checks instead of assuming they execute. Definite
   direct restricted awaits diagnose. InitSafe/DerivedSafe and compiler-registered
   bind/derive identities survive resolution; shadowed ordinary functions do not
   acquire intrinsic behavior.
7. Imports use an injected resolver returning versioned public module/symbol
   interfaces, including namespaces. No file-system assumptions in the compiler.
   Module link planning preserves legal SCCs, deterministic initialization order
   and runtime initialization-cycle checks; cycles alone are not errors.

Budgets cover CST projection depth/nodes, analysis work, symbols/captures/imports
and diagnostics. Temporary maps are discarded before returning the packed HIR.
Performance evidence will report actual large-source parse+analysis time, node/
symbol/capture storage and bounded deep/error cases, using Rust 1.96.0.

Public entry points are `analyze(&Parse, &AnalysisOptions, &mut dyn ModuleResolver)`,
`analyze_source`, `analyze_module`, and `analyze_project(&[SourceModule], ...)`.
Options declare the real module/function/generator/NoWait fragment context and
explicit host symbol descriptors. `MemoryResolver` is a fixture/reference adapter;
production resolvers own manifest/path/version policy. `ProjectAnalysis` links
source exports/reexports using a bounded worklist and retains deterministic SCC
initialization order. Imported reads keep runtime initialization-cycle checks.

Each `HirNode.form` supplies typed operand/argument/pattern/type/branch/handler
roles. Packed sequence edges retain order; original significant tokens retain
literal/string segments and exact operator spans. Trailing children is normalized
to one `Form::Call` referencing the original callee/argument list. The inner CST
call wrapper remains structural and is not a second evaluated call. Bound methods
have a separate type from plain table closures, including through aliases;
field initializers have explicit function roles, lexical captures and receivers.
Full external signatures survive analysis with name/mut/default/rest/effect/safety
metadata. Namespace references are versioned identities, not recursively copied
module trees (legal reexport cycles remain finite).

The initial checkpoint was `9723802`; implementation and source contract tests are
separate commits. The PR retains base main by owner instruction. Review only the
incremental changes after L02 `a50f1f7` (PR #9 dependency). No parser edits were needed.

Actual source checks include 24 paired author scenes in their documented context,
10 immutable accepted T01 conversions from `f92dfa2`, and 32 intact assertion
calls. Quirrel parse/analysis status is separate from `execution=waiting_runtime`.
Performance measurements and full debug/release validation are reported separately.
