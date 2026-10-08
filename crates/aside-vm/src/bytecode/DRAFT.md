# B01 v1 opcode/type draft for integration review

Frozen source: `1226c6b39dfdbd13eca665accd2ecd95bcb7d5d2:docs/aside-language.md` §22 (and §§3,7–16). Local base combines D01 `c03226a` and storage `95a7e84`. No dispatcher, frontend, heap or root-range changes.

Public data: `BytecodePackage {versions, content_version, symbols, types, constants, signatures, natives, modules, functions, capabilities, sources}`. Separate u32 newtypes: SymbolId, TypeId, ConstId, SignatureId, NativeId, ModuleId, FunctionId, GlobalId, BindingId, CaptureId, DescriptorId, RegionId, SourceId, Reg, Pc. `Const = Null | Bool | Int(i64) | FloatBits(u64) | Utf8(String) | Bytes(Vec<u8>) | Tuple(Vec<ConstId>)`; tuple references point backwards (finite immutable DAG). No Value/HeapId/token/address.

`Signature {parameters: Vec<Parameter>, rest: Option<Parameter>, return_type: TypeId, effects: Effects}`. Parameter carries name, type, mutable (default false), and optional default-expression FunctionId. A default function receives exactly the preceding ordinary parameters, runs on every binding, is Plain with NoWait context and no captures. `mut` permits rebinding only and is never an implicit reference parameter. `TypeDef` covers builtins, named module types, tuple, union and function signatures. Native declarations carry symbol, signature, ABI version and required capabilities. Host registration must match all four, and explicitly report the opcodes it executes.

`ModuleDef {path, imports, exports, globals, initializer}`; imports fix module/export/version; globals declare name/type/mutable and optional link-time function. `FunctionDef {module,name,kind,signature,context,captures,register_count,bindings,descriptors,code,regions,source_map}`. Parameters occupy the first registers. Bindings distinguish a value register from a cell register; closure descriptors capture a binding or an existing capture, with an exact Value/Cell layout. No upvalue points to a frame address.

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
| 0007 | CellNew | d,x | next | yes | - |
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
| 0045 | ArgsNamed | x,y,s | next (duplicate diagnostic) | yes | - |
| 0046 | ArgsSpread | x,y | next (capture elements now) | yes | - |
| 0047 | ArgsNamedSpread | x,y | next (capture string-keyed entries now) | yes | - |
| 0050 | MakeTuple | d,q:Sequence(window) | next | yes | - |
| 0051 | MakeArray | d,q:Sequence | next | yes | - |
| 0052 | MakeTable | d,q:Table(ordered key/value entries) | next | yes | - |
| 0053 | IndexGet | d,x,y | next | yes | - |
| 0054 | IndexSet | x,y,z | next | yes | - |
| 0055 | MemberGet | d,x,s | next (strong bound method) | yes | - |
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

Binary envelope draft: magic `ASIDBC01`, format/language/host ABI u32, content version u64, actual total byte length u32, section counts; ordered count-prefixed records with explicit tags. Reject unknown versions/tags/flags and unsupported opcode/native/capabilities, including instructions in unreachable code. Configurable input/record/string/register/work/allocation limits; scan full shape and counts before materializing, checked arithmetic, fallible reservation, then validate all references and control flow. Private VerifiedProgram owns immutable package plus capacity accounting and can only be constructed by validation. Separate format validation and explicit runtime capability validation permit tooling to inspect full language targets without claiming dispatch support.

Shared fixtures planned: ordinary main → helper → native with one register-window argument (same package for Ready/Pending); mutable cell captured by two closures and outer scope; throw/catch/finally with source spans. These are hand-authored bytecode, not claimed source compilation or runtime execution.
