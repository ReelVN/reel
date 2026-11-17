# Aside bytecode v1

The compiler and VM share this encoding. Rebuild artifacts when it changes.
Data and tags are declared in [model.rs](model.rs) and [codec.rs](codec.rs);
[spec.rs](spec.rs) defines instruction operands and [verify.rs](verify.rs)
validates packages. Author semantics are defined by the
[language specification](../../../../docs/aside-language.md).

## Envelope and records

The 68-byte header contains `ASIDBC01` (8 bytes), format/language/host ABI
(3×u32), content_version (u64), total encoded bytes (u32), and nine section
counts (9×u32). The payload begins with an optional `TextPoolDescriptor`:
u8 presence, then u64 byte length and 32 BLAKE3 digest bytes when present.

Sections are symbols, types, constants, signatures, natives, modules, functions,
capabilities and sources. Each repeats its u32 count; header/body counts must
agree. Integers are little-endian. Vectors/UTF-8 strings use u32 lengths; bool
and Option tags are u8 0/1. Enum tags are explicit u8 values. Records pack fields
without padding. IDs are separate u32 newtypes; wire and runtime PCs address u32 execution words.

Records follow model.rs field order, with these prefixes:

- Module: global count, then path/imports/exports/globals/initializer.
- Function: counts for captures/bindings/descriptors/code/regions/source_map,
  register_count, then module/name/kind/signature/context and those six tables.
  The code count is the logical instruction count; its table is a global word
  start and word length (two u32 values).
- Shared code: after modules and before functions, total words (u32), block count
  (u32), contiguous compressed `(offset, length)` pairs (two u32 each), then packed
  byte length (u32) and packed bytes. Each independent zlib block inflates to
  32 KiB, except the final block. Function ranges partition the word stream in
  declaration order. Instructions may cross a block boundary.
- Source map: u32 run count; nonempty maps add u32 packed byte length and the
  packed bytes. Each group of 32 runs restarts its previous PC, file and start
  at zero. Each run stores four shortest unsigned LEB128 zigzag deltas: PC from
  previous PC, file from previous file, start from previous start, and end from
  current start. Each difference fits the full signed difference of two u32
  values. Runtime maps retain these bytes and a sparse `(PC, byte offset)` index;
  lookup binary-searches the index and decodes at most 32 runs.
- Source file: path SymbolId (u32), original UTF-8 byte length (u32), compressed
  byte length (u32), and one independent zlib stream. Encoding uses the pinned
  compressor at level 1. Loading admits the complete original length and inflate
  workspace before allocating, validates the stream, exact length and UTF-8,
  and retains the source as ordinary text for diagnostics.

| Record | Tags |
| --- | --- |
| Const | 0 Null, 1 Bool, 2 Int64, 3 Float64Bits, 4 Utf8, 5 Bytes, 6 Tuple, 7 Text(u64 offset,u32 length) |
| TypeDef | 0 Builtin, 1 Tuple, 2 Union, 3 Function, 4 Named, 5 StringLiteral, 6 Record |
| FunctionKind | 0 Plain, 1 Async, 2 Generator, 3 Native(u32 NativeId) |
| Context | 0 Ordinary, 1 NoWait, 2 Derived, 3 Init |
| CaptureKind | 0 Value, 1 Cell |
| Descriptor | 0 Global, 1 Closure, 2 Call, 3 Sequence, 4 Table, 5 Class, 6 Super, 7 Property, 8 Pattern, 9 Spawn, 10 TableSources |
| BindingInitialization | 0 Explicit, 1 Parameter(u32) |
| ParameterDefault | 0 Constant(u32 ConstId), 1 Function(u32 FunctionId), within the parameter's Option |
| TableSources entry | 0 Pair([Reg;2]), 1 Spread(Reg) |

Execution admission accepts Plain, Generator and Native functions. The Async tag
is reserved. Generator functions bind arguments and defaults at call time, then
return a lazy generator.

Tuple constants and structural tuple/union/function types reference earlier
records, forming finite DAGs. Named types carry recursive nominal identity.
Module imports fix module/export/version. Globals retain type/mutability and
optional link-time function. Closures capture a binding or capture with exact
Value/Cell layout. Class initializers/methods and pattern defaults retain
capture-source mappings.

## Parameters and native contracts

A Signature contains ordinary parameters, optional rest, result type, effects
and optional PropertyBagTail. Each Parameter carries name, type, mutable and
optional Constant/Function default. Constants materialize through the shared
sliced loader. Function defaults receive exactly the preceding parameters and
definition captures; they are Plain/NoWait. Native Function defaults have empty
script captures. Both variants validate the resulting parameter at the call site.

Parameter registers form a prefix. Parameter(i) binding records retain their
name/type/mutability and Value(Reg(i)) or Cell(Reg(i)) storage; omitted records
mean Value storage. The binder creates and initializes parameter Cells in order.
An owner and its Function defaults share prefix storage, with Cell required for
mutable parameters. Internal default evaluation forwards those Cells; ordinary
calls create fresh invocation bindings. Native storage derives from the longest
Function-default prefix. Constant defaults contribute no prefix. VerifiedProgram
retains sparse derived Cell metadata for allocation-free storage queries.

CellNew addresses an Explicit BindingId. BindInit publishes once; BindWrite and
CellWrite validate initialization/type/mutability on the shared Cell. Its type
reference retains package version and definition module. Initialization remains
distinct from initialized null and from GC release state.

NativeDef declares canonical symbol, signature, ABI and capabilities. Native
FunctionDef reuses that signature with Ordinary context and empty script tables.
MakeClosure reads its stable rooted code value. Calls and host callbacks share
native-frame binding. An optional PC-0 source entry locates host-started calls;
script calls retain their own location.

Rest is a fresh outer array of surplus positional values. Packed name entries
hold -1, a static SymbolId integer, or an immutable GC-traced string. ArgsSpread
captures arrays/tuples positionally and own string-keyed table entries by name.
Final binding diagnoses duplicate/order conflicts after argument evaluation.
PropertyBagTail carries target parameter, schema capability and write/animation
requirements; runtime admission selects supported capabilities.

## Instructions

Execution words use this header: opcode in bits 0–7, following-word count in
bits 8–9, packed-B/C flag in bit 10, and the first operand in bits 11–31. The
21-bit all-ones first-operand value selects a full u32 in the next word. Missing
operands are zero. Nonzero B alone uses one word; B/C use either one packed pair
of u16 values or two full words. PC operands always use full words, fixing layout
before ordinal targets are mapped. Canonical headers, zero unused operands and
instruction boundaries are validated before execution.

The compiler's editable instruction occupies 16 bytes and uses ordinal PCs.
Verification maps branches, exception regions and source runs once to word PCs,
compresses the shared word stream and releases those vectors. Wire loading keeps
the same compressed blocks and validates through bounded page readers, without
an expanded instruction package. All VMs for one verified program share a lazy
8 MiB hot-page cache. A VM's fetch window reads the current page without locking;
page changes reuse the bounded cache. Frames and snapshots retain word PCs, not
page leases. A boundary bitmap admits branch, exception and restoration PCs.
`to_unpacked` is an explicit fallible editing operation that restores ordinal PCs.
Code reads and iterators propagate decompression, allocation and cache errors.

Storage statistics distinguish immutable cold capacity, actual/peak hot residency,
the hot limit and construction scratch. `PackageStats` includes actual owned
capacity; execution admission reserves the full code-capacity ceiling. All
instructions may yield at VM safe points.
The table's suspension column describes language suspension: W external wait,
Y generator yield, T thread suspend. Calls inherit cumulative context/effect
restrictions; synchronous calls are valid under NoWait.

Notation: d write Reg, x/y/z read Reg, k ConstId, s SymbolId, b BindingId,
c CaptureId, g Global descriptor, q typed descriptor, p Pc. Descriptor accessors
supply register windows and conditional writes. A range end may equal code length;
an instruction target must identify an instruction. Empty register windows may
begin at register_count.

| Hex | Opcode | operands / typed descriptor | flow | throw | suspend |
|---|---|---|---|---|---|
| 0001 | LoadConst | d,k | next | yes | - |
| 0002 | Move | d,x | next | no | - |
| 0003 | Clear | d | next (sets uninitialized bitmap) | no | - |
| 0004 | BindInit | b,x | next (one-time initialization) | yes | - |
| 0005 | BindRead | d,b | next | yes | - |
| 0006 | BindWrite | b,x | next (runtime mutable check) | yes | - |
| 0007 | CellNew | b | next (allocate uninitialized cell with BindingDef contract) | yes | - |
| 0008 | CellRead | d,x | next | yes | - |
| 0009 | CellWrite | x,y | next | yes | - |
| 000a | CaptureRead | d,c | next (returns value/cell as declared) | yes | - |
| 000b | GlobalRead | d,q:Global(module,global) | next | yes | - |
| 000c | GlobalInit | x,q:Global | next | yes | - |
| 000d | GlobalWrite | x,q:Global | next | yes | - |
| 000e | Clear2 | d,d | next (sets both slots uninitialized) | no | - |
| 000f | Clear3 | d,d,d | next (sets all three slots uninitialized) | no | - |
| 0010–001e | Add,Sub,Mul,Div,Rem,BitAnd,BitOr,BitXor,Shl,Shr,Eq,Ne,Lt,Le,Gt | d,x,y (codes in order) | next | yes | - |
| 001f | Ge | d,x,y | next | yes | - |
| 0020–0023 | Neg,Pos,Not,BitNot | d,x | next | yes | - |
| 0024 | Is | d,x,TypeId | next | yes | - |
| 0025 | TextNew | d | next | yes | - |
| 0026 | TextPush | x,y | append scalar/string to builder | yes | - |
| 0027 | TextFinish | d,x | validate and seal UTF-8 builder | yes | - |
| 0028 | CheckType | x,TypeId | next (Type on mismatch) | yes | - |
| 0030 | Jump | p | jump | no | - |
| 0031 | BranchBool | x,p,p | strict bool two-way | yes | - |
| 0032 | BranchNull | x,p,p | null/non-null two-way | no | - |
| 0040 | MakeClosure | d,q:Closure(function,capture sources) | next | yes | - |
| 0041 | Call | d,q:Call(target,window/packed args,names,optional positional contract) | next after return/resume | yes | W |
| 0042 | Return | x | return through cleanup | yes | - |
| 0043 | ArgsNew | d | next | yes | - |
| 0044 | ArgsPush | x,y | next | yes | - |
| 0045 | ArgsNamed | x,y,s | next (preserve ordered duplicate entries; final binding diagnoses) | yes | - |
| 0046 | ArgsSpread | x,y | next (capture elements now) | yes | - |
| 0047 | ArgsNamedSpread | x,y | next (capture string-keyed entries now) | yes | - |
| 0048 | CallNoWait | d,q:Call | next after synchronous return; callees inherit NoWait | yes | - |
| 0050 | MakeTuple | d,q:Sequence(window) | next | yes | - |
| 0051 | MakeArray | d,q:Sequence | next | yes | - |
| 0052 | MakeTable | d,q:Table/TableSources(ordered entries) | next | yes | - |
| 0053 | IndexGet | d,x,y | next | yes | - |
| 0054 | IndexSet | x,y,z | next | yes | - |
| 0055 | MemberGet | d,x,s | next (bind class methods only; table closures remain plain functions) | yes | - |
| 0056 | MemberSet | x,y,s | next | yes | - |
| 0057 | TableSpread | x,y | next (own slots snapshot) | yes | - |
| 0058 | ArraySpread | x,y | next (elements snapshot) | yes | - |
| 0059 | MakeClass | d,q:Class(nominal type,base,fields,methods,initializers) | next | yes | - |
| 005a | SuperGet | d,x,q:Super(lexical base,symbol) | next | yes | - |
| 005b | PropertyRef | d,x,q:Property(key,read/write) | next | yes | - |
| 005c | ChildrenPush | x,y | append composable host value to array; ignore null | yes | - |
| 005d | MakeRange | d,x,y | ascending exclusive int range | yes | - |
| 005e | MakeRangeInclusive | d,x,y | ascending inclusive int range | yes | - |
| 005f | ArrayPush | x,y | append one value to array | yes | - |
| 0060 | Match | d,x,q:Pattern(nodes,outputs) | next (bool; outputs committed on success) | yes | - |
| 0061 | PatternRequire | x | next (PatternMismatch on false) | yes | - |
| 0062 | IterInit | d,x | next | yes | - |
| 0063 | IterNext | d,x | next (internal tuple: done, value) | yes | - |
| 0064 | IterClose | x | next | yes | - |
| 0065 | AsyncIterInit | d,x | next | yes | - |
| 0066 | AsyncIterNext | d,x | next (Awaitable; explicit Await follows) | yes | - |
| 0067 | AsyncIterClose | d,x | next (Awaitable) | yes | - |
| 0068 | IterAdvance | d,x,y | writes done to d and value to y | yes | - |
| 0069 | MatchFail | - | throw MatchError | yes | - |
| 006a | SuperRef | d (captures the current method's receiver and lexical base) | next | yes | - |
| 006b | ChildrenSpread | x,y | next (capture composable children and skip null) | yes | - |
| 0070 | Throw | x | throw/unwind | yes | - |
| 0071 | Rethrow | — | throw (catch only) | yes | - |
| 0072 | Leave | p | unwind cleanup then target | yes | - |
| 0073 | EndFinally | RegionId | resume saved unwind | yes | - |
| 0080 | Yield | d,x | yields x; resumes at next instruction with null in d; Generator only | yes | Y |
| 0081 | Await | d,x | next scheduling step | yes | W |
| 0082 | Spawn | d,q:Spawn(call,owner Reg) | next (task Future) | yes | - |
| 0083 | Cancel | x,y | next (cooperative request) | yes | - |
| 0084 | ThreadNew | d,x | next | yes | - |
| 0085 | ThreadResume | d,x,y | next (synchronous driver) | yes | - |
| 0086 | ThreadSuspend | d,x | next on resume | yes | T |
| 0087 | ThreadClose | x | next | yes | - |

## Control flow and source locations

Source-map entries begin runs through the next entry or function end. A PC before
the first entry has no location. Adjacent equal spans may share one run.
Editable functions carry ordinary run vectors; verification rebases PCs and packs
them once. Loading and execution preserve the packed runs. Explicit editing via
`to_unpacked` restores the vectors and ordinal PCs. Source paths, byte spans and
debug diagnostic locations are identical across these representations.

Regions contain protected/catch/finally half-open ranges. They are laminar or
disjoint, and all blocks of one nested region share a lexical parent. Ordinary
branches/fallthrough preserve cleanup boundaries. Leave/Return run inner-to-outer
finally; faults select catch or finally through the same region tree. A fault in
catch skips that catch; a fault in finally skips that same finally. Replacement
faults retain cause chaining. Finally calls inherit NoWait; explicit suspension,
return and escaping jumps reject.

The unwind cursor records a consumed RegionId and advances only through strict
ancestors of the original PC. EndFinally requires its active matching record.
Leave preserves an enclosing source block when its target stays inside it.
`exception_handler` and `cleanup_handler` query verified metadata; the executor
owns rooted continuation state and work accounting.

Await dispatches Host objects through a trusted unary native adapter and retains
the caller result slot/source location. Completed results and pending waits use
the same scheduling and generation-checked cancellation protocol.

## Literals and collection capture

Release text pools externalize UTF-8 constants by offset/length. Binding checks
the descriptor digest, UTF-8 and character boundaries before execution. Strings
share inline or pool storage. The host binds the full immutable pool. Weak caches
reuse live constant objects; snapshots retain constant IDs for pool-backed text
and bytes for dynamic strings. Tuple constants materialize incrementally.

`TextPool::from_bytes` takes ownership of a `Vec<u8>` without copying its byte
allocation. Pools share one immutable `Arc<Vec<u8>>`; cloning retains the same
backing. Its allocation ledger includes the vector's full capacity, vector header
and shared ownership header, including unused capacity supplied by the host.

Array/TableSpread capture at their written source position with content-version
checks across pauses. Destinations remain private builders until publication.
Self-array spread duplicates its initial prefix once; self-table spread preserves
its contents. MakeTable reads ordered Pair/Spread entries; later overwrites keep
first insertion order. Empty-destination table capture copies unique entries;
nonempty merging compares against the original destination prefix in O(n*m)
comparisons, with sliced key equality.

TextPush appends scalars/strings in charged chunks. TextFinish validates and seals
UTF-8 while retaining roots and detecting concurrent mutation. Ranges use int64
endpoints and +1 steps; reversed ranges are empty, and MAX terminates safely.
IterAdvance writes done/value directly; IterNext returns their explicit tuple.
For generators, completion supplies the final return value once with done=true;
later reads supply (true, null). A yielded null has done=false. IterInit preserves
generator identity. IterClose closes it through ordinary finally unwinding;
an unstarted generator closes without entering its body. An escaping fault closes
the generator before propagating its original envelope. Generator execution
inherits the no-wait boundary; its internal work remains budget resumable.
Array iteration permits element replacement and rejects structural mutation.
ChildrenPush collects composable Host values and ignores null. ChildrenSpread
applies the same rule to each array or tuple element using sliced capture.
CallNoWait extends the restriction through ordinary helpers.

## Validation and execution admission

Decode scans counts/shape before materialization, using checked arithmetic,
fallible reservation and package-wide byte/record/register/work limits. Verify
checks references, DAGs, storage, regions and source maps. Shared scans/comparisons
admit work and scratch before traversal. Default work covers complete scan and
materialization passes; hosts can set explicit limits.

`VerifiedProgram` construction requires validation. Execution additionally checks
all opcodes, native signatures/ABI and capabilities against independently trusted
RuntimeSupport, including unreachable instructions. Format tools can inspect
representable operations before a runtime implements them. The host resolves its
own registration types/names into package tables and authorizes capabilities.
[fixtures.rs](fixtures.rs) and [testdata](testdata) retain codec/validation inputs.
