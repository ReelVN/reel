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

Issue [#37](https://github.com/jhq223/reel/issues/37) adds one public HIR field,
`Symbol.origin: BindingOrigin`, without changing the analysis entry signatures
or renumbering source FunctionId/SymbolId. `BindingOrigin` describes ownership,
not capture storage: ModuleGlobal, EntryLocal, FunctionLocal(FunctionId), External.
ModuleGlobal requires AnalysisContext::Module and a genuine root declaration;
blocks, while conditions/bodies and handlers belong to the entry. A NoWait-only
entry does not turn its declarations into globals. The centralized declaration
path maintains `Symbol.owner == Symbol.origin.function()`; owner=None is no
longer a global test. Existing consumers reading owner keep the function-owner
projection; consumers constructing Symbol literals must add origin.

EntryLocal in a module has the initializer as its frame owner. Function-parent
chains end at that entry without synthesizing a source function. Reads within
the same frame retain the uninitialized diagnostic; deferred reads retain the
INITIALIZED runtime obligation. Mutable, forward read-only and recursive
captures use the established shared Cell promotion across all prior users;
initialized immutable expression captures may retain Value mode. The capture
walk and one-time promotion scans are charged before work, and origin remains
unchanged by storage promotion. Both while condition and body bindings carry
per_iteration, including nested handlers and finally scopes. Capture limits and
symbol retained-byte accounting include the new paths/field.

External origin identifies host/import metadata only. options.enclosing remains
Local + Initialization::External with its real lexical function/entry owner,
empty external declaration range, descriptor identity and transitive captures.
It is not a fabricated global or host registration. Lowering currently has no
external Value/Cell environment link and rejects such bindings before table
admission; analysis success does not supply that link.

Algorithm:

1. Admit bounded, successful CST; project each node/token once. Keep syntax-error
   diagnostics and refuse semantic success for recovery/error CSTs.
2. For each lexical block, predeclare read-only fn bindings and pending lets/
   classes. Visit statements in order. Reads of same-frame uninitialized names
   diagnose; deferred function captures retain an initialization runtime check.
   Prepare all named function parameter/return contracts before analyzing any
   defaults or body (and prepare all class method contracts before method bodies).
   Keep parameter annotations in lexical order; mask own/later parameters during
   defaults, including nested closures, while keeping previous parameters visible.
   A default-local shadow retains its own lexical identity. Capture references
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

Current source checks include 25 paired author scenes in their documented contexts.
The 24 reviewed Quirrel migrations retain 52 original assertion call sites and
run through B01 on the real VM; parsing/analysis and execution have separate
checks. See `tests/fixtures/quirrel/README.md` for the runnable command and the
unmigrated corpus boundary.

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


Owner §22 retention contract (`7322292a32a416e6963e5b70162a25fb4e38e54a`)
was read without importing engine code or changing the author guides.
`AnalyzedModule::entry_context()` preserves the embedding context;
`Function::declared_context` preserves NoWait/Init/Derived restrictions and
`DeclaredContext::effect()` exposes the declared wait allowance. These are
separate from `known_wait` and `dynamic_calls` possibility summaries. Plain/async
bodies reset declaration context; generators, restricted methods and field
initializers declare NoWait, known derive callbacks declare Derived+NoWait.
Default expression entries require NoWait and retain their parameter node and
capture layout. Lowering must generate those entries with that restriction and
runtime must accumulate the actual caller constraints.

`AnalyzedModule::symbol_reference()` qualifies local slots with module/content
version. External descriptors preserve shared `SymbolReference` declaration
identity (canonical module/version/export), while `resolved_from` retains a
shared immutable chain of reexport module/export identities. Local import aliases
never rename the declaration identity. Namespace members and qualified annotations
use the resolver's returned canonical module/version, including custom resolvers.
A namespace references its module with `export=None`; its reexported binding has
an explicit export identity in the resolution chain.

Hosts may attach the exact trusted registration reference using
`ExternalSymbol::with_reference()`. Missing host identity remains `None` rather
than being guessed from display names. The compiler does not construct a second
native registry, infer ABI/capabilities, or fabricate default implementations.
Package linkage must resolve and validate these references against the same trusted
input, and report missing/conflicting linkage at the preserved binding/node span.
Analysis success alone does not establish native linkage or execution success.

Name-index work is admitted before cloning semantic names or allocating facts;
each predeclaration admits metadata work before function/type construction and
each symbol admits work before allocation. Block/import/signature phases stop on
exhaustion, and exhausted analyses do not allocate effect-summary graphs. The
10,002-declaration regression exercises budgets 0/1/5/100/10004/10010/20000,
including partial admission after the seeded name index. At zero budget no
semantic symbols/captures are allocated; parser/CST/HIR projection still has its
separate bounded allocation policy. `declaration_budget_probe` reports that
boundary from actual source and allocator instrumentation.


Resolver-reference normalization is idempotent for an already resolved module/
version hop. Repeating MemoryResolver→overlay→binder normalization preserves
namespace `export=None` and the exact named export of a reexport; it never derives
an export from a namespace's display name. Source regressions cover direct member
reads, ordinary symbols, aliasing namespace reexports, legal module cycles and a
namespace reexport of its own module.

`ResolutionSource` releases exclusively owned previous nodes iteratively using
`take()` and `Arc::into_inner()`. Each detached node drops with no previous edge;
a shared tail remains intact until its final owner releases it. No chain copy or
new arena is needed. The source-level 10,000-module default-budget regression
analyzes and drops its result on a 2 MiB thread stack, then repeats with a retained
shared tail. It verifies all 9,999 import-origin hops remain readable and a weak
origin reference expires after final release. Child-process isolation preserves a
useful test failure for a process-aborting stack regression.


Issue #24 default-return contract was checked against owner language commits
`65f8ba9177ab81e817202fb56fdd92bab10fe6d0` and
`8dfa3bc6dd41a192eaa7f12d0f3175cfeccfb815` without importing lowering/VM code.
A parameter default produces its value through its expression or block tail.
Explicit source return crossing that lexical boundary reports `InvalidDefault`
at the exact return statement, with a related range identifying the default.
The rule applies to conditionals, short circuits, loops, match, catch and finally
regardless of whether that path executes. An invalid return does not target the
declaring function or get compared with that function's return contract.

A private scalar default-expression marker follows ordinary lexical contexts;
source function/closure/method entry resets it, and that function's own defaults
establish their own markers. Synthesized field-initializer owners preserve the
surrounding source marker because they are not author function boundaries.
The parameter/capture default flag, runtime effect checks and public HIR layout
stay independent. No extra AST traversal or per-node marker allocation is added.

`tests/semantic_defaults.rs` checks actual parser→HIR→analysis source, including
UTF-8 statement/boundary spans, nested-function target identity, nested defaults,
diagnostic limits, previous mutable parameters, internal labeled loop transfers,
throw and rethrow. Valid defaults retain their expression type and contain no
fabricated source Return. L04 can lower that result to its compiler-generated
thunk Return as specified; this source-HIR check does not inspect or prohibit
bytecode Return. This semantic-analysis check does not execute lowering or the VM;
L04 lowering is described separately in [LOWERING.md](LOWERING.md).
Historical verification commands/results are preserved in the
[default-return boundary log](https://github.com/jhq223/reel/blob/21e0ea09ddbd6c3926df72a6689183bb73b21598/crates/aside-compiler/bench-results/semantic/default-return-boundary.txt).

## L09 call-spread/rest consistency

Call spread accepts array/tuple; known disjoint types diagnose while unknown
sources retain runtime checks. Unknown length never reserves a static argument
slot. Rest's annotation constrains the whole newly created outer array and
known incompatible annotations reject. Rest is positional-only: its declaration
name is excluded from named supply, including a collision with PropertyBagTail.
Other ordinary tail-bag behavior remains unchanged. ExternalSignature does not
yet project PropertyBagTail; this increment does not expand that interface.
Static signature checks do not execute defaults or establish runtime capture,
version conflict, allocation or error-order behavior.
