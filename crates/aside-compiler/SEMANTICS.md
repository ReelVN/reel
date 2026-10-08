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
   direct restricted awaits diagnose. Finally has a lexical prohibition: explicit
   await is rejected even in an unreachable/conditional path. Nested function
   definitions use their own execution context. InitSafe/DerivedSafe and compiler-registered
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
explicit host symbol descriptors and real lexical enclosing bindings. These
bindings have caller-supplied mutability/types, preserve capture ownership in
story fragments, and use empty declaration spans because their definitions are
outside the fragment. `MemoryResolver` is a fixture/reference adapter;
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
incremental changes after L02 `a50f1f7` (PR #9 dependency). The only L02 parser fix (separate commit) admits the `class` value category
in a type atom, as required by §3 value categories / §10 basic type annotations:
`let value: class = class {};`. No expression/control grammar changed.

Actual source checks include 24 paired author scenes in their documented context,
10 immutable accepted T01 conversions from `f92dfa2`, and 32 intact assertion
calls. Quirrel parse/analysis status is separate from `execution=waiting_runtime`.
Performance measurements and full debug/release validation are reported separately.

Limits are 1,000,000 HIR nodes, 200,000 symbols/captures, 16,000,000 work units,
10,000 imports/modules, 128 source levels/diagnostics and 32 MiB total project
source by default; callers may tighten them. Node/work/symbol/capture/import
budgets are shared across source-project modules. External descriptors/resolvers
are trusted embedding metadata; dynamic script behavior is still checked at
runtime. Returned statistics describe semantic arenas, excluding the retained
CST and allocator bookkeeping. Measured allocator/RSS evidence is separate.

The semantic result preserves canonical module path/version. Temporary child/
token/pattern vectors use pinned smallvec 1.15.1; output remains packed arenas.
Signature named lookups, export-name insertion and class-member lookup use maps;
reexports and wait/dynamic summaries use worklists. There is no repeated global
AST traversal per call, import edge or capture.

Source-review corrections preserve grammar roles through `Form::Condition`,
including an If/Block expression used as a condition. Ordinary pattern branch
failure is distinct from an impossible required binding. One shared projection
walk supplies local leaf types and exported destructuring descriptors; OR joins
only possible alternatives. Assignment checks every annotated leaf and rejects
duplicate targets. Unknown spread length does not reserve a positional slot;
runtime binding handles its positions and collisions, while definite explicit
conflicts still diagnose. Local and exported built-in annotations share a
category table (`Map` is table; `TypeHint::Closure` accepts any closure signature).
Reexport dependency edges use the resolver's actual namespace/symbol candidates.

Forward named functions have an explicit internal signature-completion state.
Contracts involving a forward signature are validated after completion, before
the analysis gate, rather than comparing a placeholder empty signature. Return
fallthrough respects conditional branches, try/finally, and loop break targets.

Call validation caches parameter projection, named lookup, rest and required
indices per function/bound-method or shared external descriptor. Empty signatures
share one canonical descriptor. Parameter projection/index construction and
remaining required-argument scans are charged before work/allocation. Repeated
empty calls to a large defaulted signature take no per-call parameter scan.
Statistics expose `signature_builds` and `signature_parameters`; this cache is
temporary and is not part of retained HIR memory.

The owner clarification in `9cfe5ece49a419c13b7f65992f8b68e46ab4ffac`
was read without importing B01 code. Finally restrictions are lexical; runtime
NoWait/Init/Derived constraints accumulate through actual calls and callbacks.
HIR check flags retain obligations and never waive the caller's restrictions.
