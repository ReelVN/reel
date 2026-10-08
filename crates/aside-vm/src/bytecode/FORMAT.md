# B01 v1 shared bytecode contract

Frozen source: `1226c6b39dfdbd13eca665accd2ecd95bcb7d5d2:docs/aside-language.md` §22 (and §§3,7–16), with owner clarifications `999a8e3860f97056dc4c7003435014a9da1166e8`, `c18db948554cdc278307617846060fe133922818`, `9cfe5ece49a419c13b7f65992f8b68e46ab4ffac` and `7322292a32a416e6963e5b70162a25fb4e38e54a`. The complete language document from the latter is preserved, including §23's subsequent runtime targets. The initial review draft was `9f36fba`; the following reflects the reviewed implementation. Local base combines D01 `c03226a` and storage `95a7e84`. No dispatcher, frontend, heap or root-range changes.

Public data: `BytecodePackage {versions, content_version, symbols, types, constants, signatures, natives, modules, functions, capabilities, sources}`. Separate u32 newtypes: SymbolId, TypeId, ConstId, SignatureId, NativeId, ModuleId, FunctionId, GlobalId, BindingId, CaptureId, DescriptorId, RegionId, SourceId, Reg, Pc. `Const = Null | Bool | Int(i64) | FloatBits(u64) | Utf8(String) | Bytes(Vec<u8>) | Tuple(Vec<ConstId>)`; tuple references point backwards (finite immutable DAG). No Value/HeapId/token/address.

`Signature {parameters: Vec<Parameter>, rest: Option<Parameter>, return_type: TypeId, effects: Effects}`. Parameter carries name, type, mutable (default false), and optional default-expression FunctionId. A default function receives exactly the preceding ordinary parameters, runs on every binding, is Plain with NoWait context and shares exactly the callee closure’s capture layout and definition ModuleInstance. Captures include lexical references in default expressions. Native defaults have no script closure environment. `mut` permits rebinding only and is never an implicit reference parameter. `TypeDef` covers builtins, named module types, tuple, union and function signatures. Native declarations carry symbol, signature, ABI version and required capabilities. Host registration must match all four, and explicitly report the opcodes it executes.

`ModuleDef {path, imports, exports, globals, initializer}`; imports fix module/export/version; globals declare name/type/mutable and optional link-time function. `FunctionDef {module,name,kind,signature,context,captures,register_count,bindings,descriptors,code,regions,source_map}`. Parameters occupy the first registers. Bindings distinguish a value register from a cell register and declare Explicit (uninitialized) or Parameter(index) initialization; closure descriptors capture a binding or an existing capture, with an exact Value/Cell layout. No upvalue points to a frame address.

Parameter(i) may use Value(Reg(i)) or Cell(Reg(i)); name/type/mutable match the signature, including rest at index parameter_count. A parameter has at most one such binding record; an omitted record retains an implicit Value slot. Each invocation initializes bindings sequentially. Cell parameters are created by the binder, never CellNew(Parameter). For each script owner/default pair, every preceding parameter has identical Value/Cell storage; mutable prefixes require Cell. Only the VM's internal default-prefix path forwards these cells. Ordinary CallTarget/Arguments, including calls to the same default FunctionDef, pass author values and create fresh invocation bindings. There is no reference-call flag or extra ABI. The verifier uses a prepaid sparse declaration index, rather than allocating function_count × parameter_count slots.

Native defaults retain the no-script-capture rule and signature prefix checks. NativeDef has no BindingDef table; a native owner's storage layout cannot be compared to the default function through the existing fields. The proposed rule for matching storage across its default thunks and requiring mutable prefix Cells is awaiting the integration owner's decision; no native binding ABI has been invented.

`Instruction {opcode: u16, flags: u16, operands: [u32;3]}` occupies exactly 16 encoded bytes, field-by-field little endian; flags and unused operands zero. PC is an instruction number. One shared `InstructionSpec` records operand kinds, reads/writes, descriptor kind, flow, may_throw and suspension category. Every instruction may BudgetYield at VM safe points. The table below lists only language suspension: `W` external wait, `Y` generator yield, `T` thread suspend, `-` none. Calls are W-capable in ordinary context but execute under the inherited dynamic effect/context boundary; NoWait contexts do not forbid a call that returns synchronously.

Notation: `d` write Reg; `x/y/z` read Reg; `k` ConstId; `s` SymbolId; `b` BindingId; `c` CaptureId; `g` Global descriptor; `q` typed descriptor; `p` Pc; unused trailing operands zero. Descriptor reads/writes are explicitly resolved by the shared accessor, including windows and pattern output registers. All potentially long structural operations can save internal budget stages.

| Hex | Opcode | operands / typed descriptor | flow | throw | suspend |
|---|---|---|---|---|---|
| 0001 | LoadConst | d,k | next | no | - |
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
| 0010–001e | Add,Sub,Mul,Div,Rem,BitAnd,BitOr,BitXor,Shl,Shr,Eq,Ne,Lt,Le,Gt | d,x,y (codes in order) | next | yes | - |
| 001f | Ge | d,x,y | next | yes | - |
| 0020–0023 | Neg,Pos,Not,BitNot | d,x | next | yes | - |
| 0024 | Is | d,x,TypeId | next | yes | - |
| 0030 | Jump | p | jump | no | - |
| 0031 | BranchBool | x,p,p | strict bool two-way | yes | - |
| 0032 | BranchNull | x,p,p | null/non-null two-way | no | - |
| 0040 | MakeClosure | d,q:Closure(function,capture sources) | next | yes | - |
| 0041 | Call | d,q:Call(target,window/packed args,names) | next after return/resume | yes | W |
| 0042 | Return | x | return through cleanup | yes | - |
| 0043 | ArgsNew | d | next | yes | - |
| 0044 | ArgsPush | x,y | next | yes | - |
| 0045 | ArgsNamed | x,y,s | next (preserve ordered duplicate entries; final binding diagnoses) | yes | - |
| 0046 | ArgsSpread | x,y | next (capture elements now) | yes | - |
| 0047 | ArgsNamedSpread | x,y | next (capture string-keyed entries now) | yes | - |
| 0050 | MakeTuple | d,q:Sequence(window) | next | yes | - |
| 0051 | MakeArray | d,q:Sequence | next | yes | - |
| 0052 | MakeTable | d,q:Table(ordered key/value entries) | next | yes | - |
| 0053 | IndexGet | d,x,y | next | yes | - |
| 0054 | IndexSet | x,y,z | next | yes | - |
| 0055 | MemberGet | d,x,s | next (bind class methods only; table closures remain plain functions) | yes | - |
| 0056 | MemberSet | x,y,s | next | yes | - |
| 0057 | TableSpread | x,y | next (own slots snapshot) | yes | - |
| 0058 | ArraySpread | x,y | next (elements snapshot) | yes | - |
| 0059 | MakeClass | d,q:Class(base,fields,methods,initializers) | next | yes | - |
| 005a | SuperGet | d,x,q:Super(lexical base,symbol) | next | yes | - |
| 005b | PropertyRef | d,x,q:Property(key,read/write) | next | yes | - |
| 0060 | Match | d,x,q:Pattern(nodes,outputs) | next (bool; outputs committed on success) | yes | - |
| 0061 | PatternRequire | x | next (PatternMismatch on false) | yes | - |
| 0062 | IterInit | d,x | next | yes | - |
| 0063 | IterNext | d,x | next (step value) | yes | - |
| 0064 | IterClose | x | next | yes | - |
| 0065 | AsyncIterInit | d,x | next | yes | - |
| 0066 | AsyncIterNext | d,x | next (Awaitable; explicit Await follows) | yes | - |
| 0067 | AsyncIterClose | d,x | next (Awaitable) | yes | - |
| 0070 | Throw | x | throw/unwind | yes | - |
| 0071 | Rethrow | — | throw (catch only) | yes | - |
| 0072 | Leave | p | unwind cleanup then target | yes | - |
| 0073 | EndFinally | RegionId | resume saved unwind | yes | - |
| 0080 | Yield | d,x | next on send; Generator only | yes | Y |
| 0081 | Await | d,x | next scheduling step | yes | W |
| 0082 | Spawn | d,q:Spawn(call,owner Reg) | next (task Future) | yes | - |
| 0083 | Cancel | x,y | next (cooperative request) | yes | - |
| 0084 | ThreadNew | d,x | next | yes | - |
| 0085 | ThreadResume | d,x,y | next (synchronous driver) | yes | - |
| 0086 | ThreadSuspend | d,x | next on resume | yes | T |
| 0087 | ThreadClose | x | next | yes | - |

Regions use `[start,end)` protected code, optional `[catch_start,catch_end)` plus exception register, optional `[finally_start,finally_end)` ending in EndFinally(region). Protected/handler blocks are laminar/disjoint, never partially overlapping or entered by arbitrary jumps. Leave exits protected/catch blocks through cleanup. Finally cannot Await/Yield/ThreadSuspend/Return or escape; calls inherit NoWait dynamically. Generator forbids explicit Await; helper/native dynamic Pending rejected by runtime context.

Binary envelope: magic `ASIDBC01`, format/language/host ABI u32, content version u64, actual total byte length u32, section counts; ordered count-prefixed records with explicit tags. Reject unknown versions/tags/flags and unsupported opcode/native/capabilities, including instructions in unreachable code. Configurable input/record/string/register/work/allocation limits; scan full shape and counts before materializing, checked arithmetic, fallible reservation, then validate all references and control flow. Private VerifiedProgram owns immutable package plus capacity accounting and can only be constructed by validation. Separate format validation and explicit runtime capability validation permit tooling to inspect full language targets without claiming dispatch support.

Decoding, verification and runtime admission independently enforce max_work. Shared signature scans, default capture equality, capability searches and trusted-registry comparisons are charged before the inner work; compressed metadata cannot hide a function × parameter × capture traversal. Uniqueness uses bounded, reserved sorted indices (including exact string sorting for hash collision groups); heap-operation comparison bounds and actual capacities are charged before work/allocation. Opcode support is normalized once. VerifiedProgram retains Limits for later runtime admission. This accounting does not change the v1 wire schema.

Normal cleanup queries use `VerifiedProgram::cleanup_handler(function, original_pc, CleanupDestination::{Return, Leave(target)}, consumed_region)`, returning only Finally HandlerTargets. They share exception_handler's all-block strict ancestor relation. Selection uses the protected/catch block containing original_pc; Leave preserves that block when target remains inside it. Runtime retains the original source/destination across finally execution and advances the consumed cursor after EndFinally. The query adds no wire records.

Shared fixtures in `fixtures.rs` and fixed wire snapshots in `testdata/`: ordinary main → helper → native with one register-window argument (same package for Ready/Pending); mutable cell captured by two closures and outer scope; throw/catch/finally with source spans; default-prefix Cell captured by a returned closure alongside an ordinary call to the same default function. These are hand-authored bytecode, not claimed source compilation or runtime execution.


## Actual wire fields

Header is 68 bytes: ASIDBC01 (8), format/language/host ABI (3×u32), content_version (u64), total bytes (u32), nine section counts (9×u32). Sections, each count-prefixed again, are symbols, types, constants, signatures, natives, modules, functions, capabilities and sources. Header and body counts must agree in the scan before payload allocation. All integers use little endian. Vectors/strings use u32 counts; strings contain strict UTF-8 bytes. bool and Option tags are u8 0/1. Enums use explicit u8 tags from codec.rs, never Rust discriminant layout. Unknown tags are rejected. No padding or alignment bytes.

Records use model.rs field order except ModuleDef and FunctionDef: module records start with an additional global count, then path/imports/exports/globals/initializer. Function records start with six counts (captures/bindings/descriptors/code/regions/source_map), register_count, then module/name/kind/signature/context and those six tables. PC range ends may equal code length; instruction targets may not. Empty register windows may begin at register_count.

Constants: 0 Null, 1 Bool, 2 Int64, 3 Float64Bits, 4 Utf8, 5 Bytes, 6 Tuple. Type tags: 0 Builtin, 1 Tuple, 2 Union, 3 Function, 4 Named. FunctionKind: 0 Plain, 1 Async, 2 Generator. Context: 0 Ordinary, 1 NoWait, 2 Derived, 3 Init. CaptureKind: 0 Value, 1 Cell. Descriptor tags: 0 Global, 1 Closure, 2 Call, 3 Sequence, 4 Table, 5 Class, 6 Super, 7 Property, 8 Pattern, 9 Spawn. BindingInitialization: 0 Explicit, 1 Parameter(u32). Remaining tag numbers are explicit in codec.rs.

Signature additionally carries optional PropertyBagTail (target parameter, schema capability, writable/animatable requirements). Unknown names in this mode reach the actual target schema after ordinary parameters bind. Class field initializers/methods and pattern defaults carry capture-source mappings, preserving local lexical semantics. Ordinary structural types are backwards-referenced DAGs; a function-type/signature cycle is rejected, while Named types permit recursive class identity.

CellNew(BindingId) resolves the destination Cell register from BindingDef and associates the complete BindingDef with an uninitialized cell; BindInit publishes its initial value once. BindWrite and CellWrite both validate initialized state, declared type and mutable permission on the same shared cell. Match declares ConditionalWrite outputs; failed matching never initializes them. ArgsNamed/ArgsNamedSpread preserve repeated names and complete subsequent argument evaluation; the final call binder diagnoses conflicts. Calls copy argument values before committing an overlapping destination and inherit the active effect/wait context.

Normal Jump/Branch/fallthrough cannot enter catch/finally or bypass cleanup on protected/catch exit. Leave records the destination and runs innermost cleanup; Return/Throw use the same unwind state. exception_handler resolves fault edges: protected code targets its catch (or finally), a catch fault skips that catch and first runs its finally, a finally fault skips that same finally and reaches outer handlers. EndFinally requires the matching active unwind record (static terminator validation plus runtime check). New cleanup faults replace pending results with cause chaining.

`InstructionSpec::semantic_rule`, `unwind`, `visit_registers` and `VerifiedProgram::exception_handler` are shared integration APIs; dispatchers must preserve these contracts. Static verification is separate from actual runtime support: use `VerifiedProgram::load(bytes, limits, support)` at execution load. NativeSupport signatures are trusted adapter snapshots whose names/types have been independently resolved against this package’s tables; copying package declarations does not verify a host registry.

Cell type references retain the owning package content version and definition module along with TypeId; the runtime stores that fixed reference with the cell. GC release Option state is separate from the language initialization bit. Capturing a cell reads only its handle, never the uninitialized content. The runtime suite must distinguish initialized null from unset initialization, reject second BindInit and readonly CellWrite, and reject or route ordinary set_field(Cell) through the same checked write path.

The unwind cursor is a consumed RegionId with strict-ancestor semantics, validated against the original instruction PC of this unwind. Once an outer region has been consumed, its inner regions cannot become targets again. Protected/catch/finally blocks belonging to one region must share a lexical parent: if any block lies inside another region’s block, all of them must lie inside that same block. Nested Leave inside finally is legal while its destination remains inside that finally.

NativeSupport includes required_capabilities from the trusted host registry. Every trusted requirement must be authorized by RuntimeSupport and declared by both the package and its native record. Removing untrusted capability declarations cannot grant access. Snapshot generation must use the independent registry, never copy package declarations to manufacture support.

Use visit_registers_budgeted at untrusted boundaries: it precharges register windows, capture arrays, class lists and pattern metadata before traversal, and can interrupt scalar visits. Verification also charges scratch lengths before allocation/fill and bulk pattern-stack extensions before copying. The unmetered visit_registers adapter is for previously trusted analysis.
