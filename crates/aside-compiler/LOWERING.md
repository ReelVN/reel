# L04 lowering contract (IR v1, bytecode v1)

Issue #24 consumes L03 `AnalyzedModule`; it does not analyze a second semantic
model or execute programs. The language baseline is executor commit
`7322292a32a416e6963e5b70162a25fb4e38e54a:docs/aside-language.md`.
The integration branch preserves L03 `962573bd856bafd01c51c190bc19b6f73f0b069b`
and B01 `35568323f84322d2bfa8b1e6fa1914f81d710618` as actual parents.
Final dependency updates will be merged, not copied into competing APIs.

Allowed edits: aside-compiler and its Cargo.lock dependency entry. The initial
dependency merge resolves language/creator documents to the owner baseline.
No VM dispatcher, runtime execution tests, Actions, merge, ready transition or
acceptance comment belongs to this task.

The IR reuses B01 instructions, bindings, captures, types and descriptors.
It adds ordered basic blocks, explicit terminators and exact source spans.
Parameters occupy fixed prefix slots. Lexical bindings retain stable slots;
temporary slots and contiguous call windows are reclaimed when their last
lowering use ends. The callee and all actual arguments are evaluated before
the single Call instruction. The binder, not the compiler, runs defaults.

Default functions receive the preceding parameters with the same binding
storage/type/mutability and the body's definition capture layout. Parameter
Cells are created by the call binder, never replaced by a body CellNew.
Native identities, ABI, capabilities and default implementations come from an
explicit trusted verified linkage package. An external default-presence flag
does not provide an implementation. Native values remain a later callable task.

Try lowering records shared exception regions and emits Return/Leave/EndFinally;
cleanup selection belongs to B01/V03. All emitted PCs have source maps.
Unknown lowering nodes and absent links fail with a source range, never a
placeholder null success. Author annotations supply runtime type contracts;
inferred function types do not create structural signature contracts.

Work, vector/string capacity, blocks, instructions, tables and register counts
are charged before traversal, reservation or emission. Source/HIR and caller
link inputs are borrowed; new retained and scratch storage are accounted
together. Compiler checks and bytecode checks use independent limits.

Validation will separately record compile/verify/codec/disassembly checks,
register liveness/order/maps, resource exhaustion and release measurements.
Execution acceptance remains pending the independent V03 work. This is a
partial implementation of the full language, not a language completion claim.
