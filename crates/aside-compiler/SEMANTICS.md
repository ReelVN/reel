# L03 semantic interface v1 (review checkpoint)

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
3. Mutable captured bindings use one shared symbolic cell across all closures;
   immutable captures retain object identity by value. Loop binding scopes carry
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
   execute bodies. NoWait constraints are checked at call sites after inference.
7. Imports use an injected resolver returning versioned public module/symbol
   interfaces, including namespaces. No file-system assumptions in the compiler.
   Module link planning preserves legal SCCs, deterministic initialization order
   and runtime initialization-cycle checks; cycles alone are not errors.

Budgets cover CST projection depth/nodes, analysis work, symbols/captures/imports
and diagnostics. Temporary maps are discarded before returning the packed HIR.
Performance evidence will report actual large-source parse+analysis time, node/
symbol/capture storage and bounded deep/error cases, using Rust 1.96.0.

This checkpoint publishes the representation and algorithm before the analyzer.
Implementation and acceptance results follow in separate commits on this draft PR.
