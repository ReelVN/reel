macro_rules! indices {
    ($($name:ident),+ $(,)?) => { $(
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(transparent)]
        #[derive(serde::Serialize, serde::Deserialize)]
        pub struct $name(pub u32);
        impl $name { pub fn index(self) -> usize { self.0 as usize } }
    )+ };
}
indices!(
    SymbolId,
    TypeId,
    ConstId,
    SignatureId,
    NativeId,
    ModuleId,
    FunctionId,
    GlobalId,
    BindingId,
    CaptureId,
    DescriptorId,
    RegionId,
    SourceId,
    Reg,
    Pc
);

pub const FORMAT_VERSION: u32 = 1;
pub const LANGUAGE_VERSION: u32 = 1;
pub const HOST_ABI_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BytecodePackage {
    pub format_version: u32,
    pub language_version: u32,
    pub host_abi_version: u32,
    pub content_version: u64,
    pub text_pool: Option<TextPoolDescriptor>,
    pub symbols: Vec<String>,
    pub types: Vec<TypeDef>,
    pub constants: Vec<Const>,
    pub signatures: Vec<Signature>,
    pub natives: Vec<NativeDef>,
    pub modules: Vec<ModuleDef>,
    pub functions: Vec<FunctionDef>,
    pub capabilities: Vec<SymbolId>,
    pub sources: Vec<SourceFile>,
}
impl Default for BytecodePackage {
    fn default() -> Self {
        Self {
            format_version: FORMAT_VERSION,
            language_version: LANGUAGE_VERSION,
            host_abi_version: HOST_ABI_VERSION,
            content_version: 1,
            text_pool: None,
            symbols: Vec::new(),
            types: Vec::new(),
            constants: Vec::new(),
            signatures: Vec::new(),
            natives: Vec::new(),
            modules: Vec::new(),
            functions: Vec::new(),
            capabilities: Vec::new(),
            sources: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Const {
    Null,
    Bool(bool),
    Int(i64),
    FloatBits(u64),
    Utf8(String),
    Text { offset: u64, len: u32 },
    Bytes(Vec<u8>),
    Tuple(Vec<ConstId>),
}

/// Identity of the immutable UTF-8 backing supplied by the host, independently
/// of its file/container name or the other resources shipped beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextPoolDescriptor {
    pub byte_len: u64,
    pub digest: [u8; 32],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BuiltinType {
    Any,
    Null,
    Bool,
    Int,
    Float,
    String,
    Bytes,
    Array,
    Table,
    Tuple,
    Class,
    Instance,
    Closure,
    Generator,
    Thread,
    Future,
    WeakRef,
    /// A bind/derive property reference; access permissions belong to the value.
    PropertyRef,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeDef {
    Builtin(BuiltinType),
    Tuple(Vec<TypeId>),
    Union(Vec<TypeId>),
    /// A positional call contract; parameter names belong to the actual callee.
    Function {
        parameters: Vec<TypeId>,
        return_type: TypeId,
    },
    Named {
        module: ModuleId,
        name: SymbolId,
        version: u64,
    },
}
/// A mask of allowed effects, not a promise that a call will actually wait.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Effects(pub u32);
impl Effects {
    pub const ALLOCATE: u32 = 1;
    pub const WRITE: u32 = 2;
    pub const HOST: u32 = 4;
    pub const WAIT: u32 = 8;
    pub const ALL: Self = Self(15);
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parameter {
    pub name: SymbolId,
    pub ty: TypeId,
    /// Rebinding permission. Passing a parameter always uses ordinary value semantics.
    pub mutable: bool,
    pub default: Option<ParameterDefault>,
}
/// An omitted argument either reads an immutable pool value or evaluates a
/// function against the callee's preceding parameters and lexical captures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterDefault {
    Constant(ConstId),
    Function(FunctionId),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    pub parameters: Vec<Parameter>,
    pub rest: Option<Parameter>,
    pub return_type: TypeId,
    pub effects: Effects,
    /// Remaining names are validated against the actual target's host schema.
    pub property_bag_tail: Option<PropertyBagTail>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropertyBagTail {
    pub target_parameter: u32,
    pub capability: Option<SymbolId>,
    pub require_writable: bool,
    pub require_animatable: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeDef {
    pub symbol: SymbolId,
    pub signature: SignatureId,
    pub abi_version: u32,
    pub capabilities: Vec<SymbolId>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalDef {
    pub name: SymbolId,
    pub ty: TypeId,
    pub mutable: bool,
    pub linked_function: Option<FunctionId>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    pub name: SymbolId,
    pub module: ModuleId,
    pub export: SymbolId,
    pub content_version: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    pub name: SymbolId,
    /// Module binding index: owned globals followed by imported bindings.
    pub global: GlobalId,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleDef {
    pub path: SymbolId,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub globals: Vec<GlobalDef>,
    pub initializer: Option<FunctionId>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FunctionKind {
    Plain,
    Async,
    Generator,
    /// A callable registration, prepared directly as a native frame.
    Native(NativeId),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    Ordinary,
    NoWait,
    Derived,
    Init,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureKind {
    Value,
    Cell,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BindingStorage {
    Value(Reg),
    Cell(Reg),
}
impl BindingStorage {
    pub fn reg(self) -> Reg {
        match self {
            Self::Value(r) | Self::Cell(r) => r,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingInitialization {
    Explicit,
    /// The binder establishes Value(Reg(i)) or one Cell(Reg(i)) per invocation.
    /// Only the internal default-prefix path reuses the caller's parameter cell;
    /// ordinary calls, including calls to a default FunctionDef, bind fresh values.
    Parameter(u32),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingDef {
    pub name: SymbolId,
    pub ty: TypeId,
    pub mutable: bool,
    pub storage: BindingStorage,
    /// Explicit bindings start uninitialized; BindInit publishes exactly once.
    /// Parameter bindings are initialized sequentially before later defaults and
    /// body execution. Owner/default prefixes must share name/type/mutable/storage.
    pub initialization: BindingInitialization,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionDef {
    pub module: ModuleId,
    pub name: SymbolId,
    pub kind: FunctionKind,
    pub signature: SignatureId,
    pub context: Context,
    pub captures: Vec<CaptureKind>,
    pub register_count: u32,
    pub bindings: Vec<BindingDef>,
    pub descriptors: Vec<Descriptor>,
    pub code: Vec<Instruction>,
    pub regions: Vec<ExceptionRegion>,
    pub source_map: Vec<SourceMapEntry>,
}
/// Fixed-width execution record; the wire codec stores only declared operands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub opcode: u16,
    pub flags: u16,
    pub operands: [u32; 3],
}
impl Instruction {
    pub const fn new(op: super::Opcode, a: u32, b: u32, c: u32) -> Self {
        Self {
            opcode: op as u16,
            flags: 0,
            operands: [a, b, c],
        }
    }
}
const _: () = assert!(std::mem::size_of::<Instruction>() == 16);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RegWindow {
    pub start: Reg,
    pub len: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureSource {
    Binding(BindingId),
    Capture(CaptureId),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallTarget {
    Register(Reg),
    Function(FunctionId),
    Native(NativeId),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arguments {
    Window(RegWindow),
    Packed(Reg),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedArg {
    pub name: SymbolId,
    pub offset: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallDesc {
    pub target: CallTarget,
    pub arguments: Arguments,
    pub names: Vec<NamedArg>,
    /// Checks positional arguments; CheckType validates the result at the caller.
    pub contract: Option<TypeId>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassField {
    pub name: SymbolId,
    pub ty: TypeId,
    pub mutable: bool,
    pub is_static: bool,
    pub initializer: Option<FunctionId>,
    pub captures: Vec<CaptureSource>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassMethod {
    pub name: SymbolId,
    pub function: FunctionId,
    pub is_static: bool,
    pub captures: Vec<CaptureSource>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassDesc {
    pub nominal: Option<TypeId>,
    pub base: Option<Reg>,
    pub fields: Vec<ClassField>,
    pub methods: Vec<ClassMethod>,
}
/// Pattern nodes form a backwards-referenced DAG; matching uses explicit VM stages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatternNode {
    Wildcard,
    Bind {
        name: SymbolId,
        output: Reg,
    },
    Literal(ConstId),
    Tuple {
        items: Vec<u32>,
        rest: Option<u32>,
    },
    Array {
        items: Vec<u32>,
        rest: Option<u32>,
    },
    Object {
        fields: Vec<PatternField>,
        rest: Option<u32>,
    },
    Is {
        node: u32,
        ty: TypeId,
    },
    Or {
        left: u32,
        right: u32,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternField {
    pub name: SymbolId,
    pub node: u32,
    pub default: Option<FunctionId>,
    pub captures: Vec<CaptureSource>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternDesc {
    pub nodes: Vec<PatternNode>,
    pub root: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableSource {
    Pair([Reg; 2]),
    Spread(Reg),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Descriptor {
    Global {
        module: ModuleId,
        global: GlobalId,
    },
    Closure {
        function: FunctionId,
        captures: Vec<CaptureSource>,
    },
    Call(CallDesc),
    Sequence(RegWindow),
    Table(Vec<[Reg; 2]>),
    TableSources(Vec<TableSource>),
    Class(ClassDesc),
    Super {
        base: Reg,
        member: SymbolId,
    },
    Property {
        key: Reg,
        writable: bool,
    },
    Pattern(PatternDesc),
    Spawn {
        call: CallDesc,
        owner: Reg,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorKind {
    Global,
    Closure,
    Call,
    Sequence,
    Table,
    Class,
    Super,
    Property,
    Pattern,
    Spawn,
}
impl Descriptor {
    pub fn kind(&self) -> DescriptorKind {
        match self {
            Self::Global { .. } => DescriptorKind::Global,
            Self::Closure { .. } => DescriptorKind::Closure,
            Self::Call(_) => DescriptorKind::Call,
            Self::Sequence(_) => DescriptorKind::Sequence,
            Self::Table(_) | Self::TableSources(_) => DescriptorKind::Table,
            Self::Class(_) => DescriptorKind::Class,
            Self::Super { .. } => DescriptorKind::Super,
            Self::Property { .. } => DescriptorKind::Property,
            Self::Pattern(_) => DescriptorKind::Pattern,
            Self::Spawn { .. } => DescriptorKind::Spawn,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcRange {
    pub start: Pc,
    pub end: Pc,
}
impl PcRange {
    pub fn contains(self, pc: Pc) -> bool {
        self.start <= pc && pc < self.end
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatchRegion {
    pub range: PcRange,
    pub exception: Reg,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExceptionRegion {
    pub protected: PcRange,
    pub catch: Option<CatchRegion>,
    pub finally: Option<PcRange>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFile {
    pub path: SymbolId,
    pub text: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceSpan {
    pub file: SourceId,
    pub start: u32,
    pub end: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceMapEntry {
    pub pc: Pc,
    pub span: SourceSpan,
}

/// Dispatcher/host supplied capabilities. Empty opcodes mean no execution support.
#[derive(Clone, Debug, Default)]
pub struct RuntimeSupport {
    pub opcodes: Vec<super::Opcode>,
    pub capabilities: Vec<String>,
    pub natives: Vec<NativeSupport>,
    /// Trusted nominal host declarations, supplied independently of the package.
    pub host_types: Vec<crate::HostTypeSupport>,
}
#[derive(Clone, Debug)]
pub struct NativeSupport {
    pub symbol: String,
    pub abi_version: u32,
    /// Exact signature including parameter mutability and effect contract.
    /// The trusted adapter resolves parameter names and types against this
    /// program's tables using its independent native registration metadata.
    /// Copying an untrusted package signature is not registry validation.
    pub signature: Signature,
    /// Required by the trusted host registration, independent of package claims.
    pub required_capabilities: Vec<String>,
}
