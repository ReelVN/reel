# Aside compiler

`aside-compiler` scans and parses source into lossless `aside-syntax` trees,
binds source-owned typed HIR, then lowers it to register IR and verified
`aside-vm` bytecode. The [language specification](../../docs/aside-language.md)
defines syntax and execution semantics; the [host SDK](../../docs/aside-host.md)
defines trusted native declarations.

## Frontend and diagnostics

[`lex`, `parse` and `parse_with_options`](src/lib.rs) accept UTF-8 source.
`EntryPoint::{Module, Block, Expression, Pattern, Type}` shares one parser;
fragment ranges remain relative to the supplied text. The tree preserves all
admitted source bytes, including whitespace, comments and error tails. Missing
tokens produce empty-range diagnostics. `SourceMap` provides byte, scalar and
UTF-16 coordinates; the caller supplies file and module identity.

Source/token capacities follow the u32 representation. Recursive depth and
diagnostic limits default to 128; hosts can set quotas. Exhaustion reports
`ASR001` and retains the remaining source. Flat binary/postfix chains use
iterative traversal. Pattern trials borrow the same tokens, share a work budget
and select destructuring only after a complete pattern followed by `=`.
See [`parser`](src/parser.rs) and [`diagnostic`](src/diagnostic.rs).

## Binding and effects

[`semantic`](src/semantic) exposes `analyze`, `analyze_module` and
`analyze_project`. `AnalysisOptions` supplies execution context, host symbols and
enclosing bindings; `ModuleResolver` supplies canonical module/version identity.
Successful analysis produces the `AnalyzedModule` required by lowering.

- Read-only function bindings and pending declarations are predeclared; source
  execution initializes them in order. Same-frame reads before initialization
  diagnose; deferred captures retain a runtime initialization check.
- `BindingOrigin` distinguishes module globals, initializer locals, function
  locals and external metadata. Mutable, forward and recursive captures share
  Cells across every user. Reentered loop scopes create fresh captured storage.
- Defaults see preceding parameters and definition-site captures. Own/later
  parameters remain unavailable. Explicit `return` crossing a default boundary
  diagnoses; its expression or block tail supplies the value.
- Class methods have explicit `self`; `super` retains its lexical base. Stored
  table closures use ordinary function calls. Imported class/type identities
  remain canonical through aliases and reexports.
- `NoWait`, `Init` and `Derived` restrictions accumulate along actual calls and
  host callbacks. A call that completes synchronously is valid under NoWait;
  actual suspension is rejected. Explicit awaits in finally diagnose even on
  conditional paths. Nested function bodies have their own declared context.
- Known type/signature conflicts diagnose at source spans; dynamic facts retain
  runtime checks. Named function contracts are prepared before body analysis.
  Unannotated parameters and return contracts are dynamic.
- Module linking preserves legal cycles and deterministic initialization order.
  Reads of uninitialized imports retain runtime checks.

The binding implementation is in [`semantic/bind`](src/semantic/bind.rs), with
shared signature projection in [`semantic/types`](src/semantic/types.rs) and
module resolution in [`semantic/link`](src/semantic/link.rs).

## Lowering and execution

[`lowering::lower`](src/lowering/mod.rs) consumes analyzed HIR and trusted
`Linkage`; `IrModule::emit` returns verified bytecode, source metadata, entry and
statistics. Native linkage checks canonical identity, signature, ABI and
capabilities. Compiler-generated PCs retain the author's source ranges.

- Calls evaluate the callee and actual arguments once in source order. The VM
  binder runs defaults after argument capture. Literal defaults use
  `ParameterDefault::Constant`; expression defaults use `Function` with the
  exact preceding Value/Cell layout and definition captures.
- Assignments prepare locations left to right, evaluate the RHS once and write
  in reverse order. Compound assignments retain their original receiver/key,
  read once before the RHS and write back through that saved location.
- Fixed collection elements and table key/value expressions evaluate in source
  order. Spreads capture at their written position. Arrays/tuples supply
  positional call arguments; own string-keyed table entries supply named ones.
  Rest receives a fresh outer array of surplus positional values.
- Optional chains branch to one null result before evaluating skipped keys or
  arguments. Grouping ends propagation. Same-chain assignment targets reject.
- Return/leave/throw use shared exception regions and finally cleanup. Finally
  runs inner to outer; terminal paths omit later construction and writes.
- Children blocks collect composable host values, ignoring null, through normal
  block/loop flow. Nested collectors are independent. Helpers inherit NoWait.
- Ordinary functions, closures, classes, modules, collections, fixed tuple
  destructuring, scalar/type/OR match arms, host awaits and callbacks execute on
  the common VM. Array/object/rest structural patterns, destructuring assignment,
  structural match patterns, generators/threads, delegates/hooks
  and property-bag tails retain explicit unsupported diagnostics/admission.
- The current bytecode encoding includes an Async function tag; lowering and
  execution admission do not support it. Ordinary functions wait through the
  common call stack.
- External fragment bindings require a Value/Cell environment link; lowering
  rejects an absent link at the source range.

Implementation is split across [`lowering/build`](src/lowering/build),
[`registers`](src/lowering/registers.rs), [`constants`](src/lowering/constants.rs)
and [`project`](src/lowering/project.rs). Work and retained/scratch capacity are
admitted before traversal and allocation; errors retain source spans and stats.

## Checks

```sh
cargo test --locked -p aside-compiler
cargo clippy --locked -p aside-compiler --all-targets -- -D warnings
cargo run --locked -p aside-compiler --example check -- path/to/file.aside module
cargo run --locked -p reel-dev -- compiler-probe lowering
```

[`tests/execution.rs`](tests/execution.rs) executes source and decoded bytecode
with interleaved GC. Class/module/host-awaitable tests cover their own execution
boundaries; lowering tests inspect operands, liveness and source maps.
[Bilingual guide checks](tests/guide.rs) use registered execution contexts.
[Quirrel fixtures](tests/fixtures/quirrel/README.md) retain original oracles.
