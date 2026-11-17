//! L03 HIR v1: source-owned, arena-based semantic input to future lowering.
//! This module does not define bytecode, VM handles, or executed behavior.
use crate::{SyntaxKind, TextRange};
use std::{ops::Range, sync::Arc};

macro_rules! id {
    ($($name:ident),*) => {$ (
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u32);
    )*};
}
id!(NodeId, NameId, ScopeId, SymbolId, FunctionId, TypeId);

/// Every non-error CST production has a semantic production. Significant tokens
/// remain ordered with nodes, preserving operators, field names and punctuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirKind {
    /// An incomplete editor production. Strict compilation never projects it.
    Error,
    Module,
    Block,
    Let,
    Function,
    Class,
    Import,
    ImportPath,
    ImportGroup,
    Parameters,
    Parameter,
    NamedType,
    StringType,
    RecordType,
    RecordFieldType,
    TupleType,
    UnionType,
    FunctionType,
    GroupType,
    ExpressionStatement,
    Return,
    Break,
    Continue,
    Throw,
    Yield,
    Name,
    Literal,
    String,
    Interpolation,
    Tuple,
    Group,
    Array,
    Object,
    ObjectField,
    Spread,
    Prefix,
    Binary,
    Range,
    Assignment,
    Member,
    Index,
    Call,
    Arguments,
    Argument,
    NamedArgument,
    Children,
    Closure,
    If,
    While,
    For,
    Loop,
    Label,
    Match,
    MatchArm,
    Try,
    Catch,
    Finally,
    NamePattern,
    WildcardPattern,
    LiteralPattern,
    TuplePattern,
    GroupPattern,
    ArrayPattern,
    ObjectPattern,
    PatternField,
    RestPattern,
    OrPattern,
    TypePattern,
    ModifiedDeclaration,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HirToken {
    pub kind: SyntaxKind,
    pub span: TextRange,
    /// Identifiers are interned once per module. Other text is a source slice.
    pub name: Option<NameId>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirElement {
    Node(NodeId),
    Token(HirToken),
}
#[derive(Debug, Clone)]
pub struct HirNode {
    pub kind: HirKind,
    pub span: TextRange,
    pub parent: Option<NodeId>,
    pub elements: Range<u32>,
    pub form: Form,
}
#[derive(Debug, Clone)]
pub struct Hir {
    pub source: Arc<str>,
    pub nodes: Vec<HirNode>,
    pub elements: Vec<HirElement>,
    pub names: Vec<String>,
}
impl Hir {
    pub fn node(&self, id: NodeId) -> &HirNode {
        &self.nodes[id.0 as usize]
    }
    pub fn elements(&self, id: NodeId) -> &[HirElement] {
        let r = &self.node(id).elements;
        &self.elements[r.start as usize..r.end as usize]
    }
    pub fn children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.elements(id).iter().filter_map(|e| match e {
            HirElement::Node(n) => Some(*n),
            _ => None,
        })
    }
    pub fn tokens(&self, id: NodeId) -> impl Iterator<Item = HirToken> + '_ {
        self.elements(id).iter().filter_map(|e| match e {
            HirElement::Token(t) => Some(*t),
            _ => None,
        })
    }
    pub fn name(&self, id: NameId) -> &str {
        &self.names[id.0 as usize]
    }
    pub fn text(&self, span: TextRange) -> &str {
        &self.source[crate::byte_range(span)]
    }
    /// A lexical statement or match arm collects its direct value, while
    /// control-flow bodies collect their own expressions.
    pub(crate) fn collects(&self, mut node: NodeId) -> bool {
        while let (HirKind::Group, Form::Single { child }) =
            (self.node(node).kind, self.node(node).form)
        {
            node = child;
        }
        !matches!(
            self.node(node).kind,
            HirKind::Assignment
                | HirKind::Block
                | HirKind::If
                | HirKind::Match
                | HirKind::For
                | HirKind::While
                | HirKind::Loop
                | HirKind::Try
                | HirKind::Return
                | HirKind::Throw
                | HirKind::Break
                | HirKind::Continue
                | HirKind::Label
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Any,
    Closure,
    Null,
    Bool,
    Int,
    Float,
    String,
    Array,
    Table,
    Future,
    Generator,
    Tuple(Vec<TypeId>),
    Union(Vec<TypeId>),
    Callable(Vec<TypeId>, TypeId),
    Function(FunctionId),
    BoundMethod(FunctionId),
    Class(NodeId),
    Instance(NodeId),
    Named(NameId),
    StringLiteral(String),
    Record(Vec<(NameId, TypeId)>),
}
/// A record, optionally wrapped in a nullable annotation, has one closed field set.
pub fn record_fields(types: &[Type], ty: TypeId) -> Option<&[(NameId, TypeId)]> {
    match &types[ty.0 as usize] {
        Type::Record(fields) => Some(fields),
        Type::Union(members)
            if members.len() == 2
                && members.iter().any(|id| types[id.0 as usize] == Type::Null) =>
        {
            members.iter().find_map(|id| match &types[id.0 as usize] {
                Type::Record(fields) => Some(fields.as_slice()),
                _ => None,
            })
        }
        _ => None,
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Local,
    Parameter,
    Function,
    Class,
    Import,
    Host,
    Catch,
    Receiver,
    Super,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    Local,
    Module,
    ValueCapture,
    SharedCell,
}
/// SharedCell carries this state on the cell itself, including read-only cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Initialization {
    ScopeEntry,
    Declaration,
    Parameter,
    External,
}
/// Lexical ownership, independent of ValueCapture/SharedCell promotion.
/// EntryLocal belongs to this analyzed entry: lowering currently admits it
/// only for a module initializer, never as a module global.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingOrigin {
    ModuleGlobal,
    EntryLocal,
    FunctionLocal(FunctionId),
    /// Host/import metadata. An enclosing lexical binding is still local,
    /// with Initialization::External, and can be captured through functions.
    External,
}
impl BindingOrigin {
    pub fn function(self) -> Option<FunctionId> {
        match self {
            Self::FunctionLocal(id) => Some(id),
            _ => None,
        }
    }
    pub(crate) fn same_frame(self, function: Option<FunctionId>) -> bool {
        match self {
            Self::ModuleGlobal | Self::EntryLocal => function.is_none(),
            Self::FunctionLocal(id) => function == Some(id),
            Self::External => false,
        }
    }
}
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: NameId,
    pub declaration: TextRange,
    pub scope: ScopeId,
    pub origin: BindingOrigin,
    /// Compatibility projection of origin.function(); None does not mean global.
    pub owner: Option<FunctionId>,
    pub kind: SymbolKind,
    pub mutable: bool,
    pub ty: TypeId,
    pub storage: Storage,
    pub initialization: Initialization,
    /// Allocate this binding (and any cell) afresh on each loop iteration.
    pub per_iteration: bool,
}
#[derive(Debug, Clone)]
pub struct Scope {
    pub parent: Option<ScopeId>,
    pub owner: Option<FunctionId>,
    pub span: TextRange,
    pub symbols: Vec<SymbolId>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionKind {
    Plain,
    Async,
    Generator,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Receiver {
    None,
    ExplicitSelf,
    Static,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionRole {
    Body,
    FieldInitializer,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    Value,
    Cell,
}
#[derive(Debug, Clone)]
pub struct Capture {
    pub symbol: SymbolId,
    pub mode: CaptureMode,
    pub first_use: TextRange,
    /// Defaults are evaluated at call time using this definition environment.
    pub used_in_default: bool,
}
#[derive(Debug, Clone)]
pub struct Parameter {
    pub symbol: SymbolId,
    pub annotation: Option<TypeId>,
    pub default: Option<NodeId>,
    pub rest: bool,
}
/// Declared restrictions, independent of inferred possibility summaries and
/// caller constraints. Runtime combines these with the effective calling context.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeclaredContext {
    pub no_wait: bool,
    pub init: bool,
    pub derived: bool,
}
impl DeclaredContext {
    pub fn effect(self) -> crate::semantic::CallEffect {
        if self.no_wait {
            crate::semantic::CallEffect::NoWait
        } else {
            crate::semantic::CallEffect::MayWait
        }
    }
}
#[derive(Debug, Clone)]
pub struct Function {
    pub node: NodeId,
    pub parent: Option<FunctionId>,
    pub kind: FunctionKind,
    pub receiver: Receiver,
    pub role: FunctionRole,
    pub declared_context: DeclaredContext,
    pub lexical_class: Option<NodeId>,
    pub parameters: Vec<Parameter>,
    pub return_type: TypeId,
    pub body: NodeId,
    pub captures: Vec<Capture>,
    pub known_wait: bool,
    pub dynamic_calls: bool,
}
/// Facts are indexed by NodeId. Unknown facts keep runtime checks; they are not errors.
#[derive(Debug, Clone)]
pub struct NodeFacts {
    pub ty: TypeId,
    pub symbol: Option<SymbolId>,
    pub function: Option<FunctionId>,
    pub scope: Option<ScopeId>,
    pub checks: RuntimeChecks,
    pub control: Option<ControlTransfer>,
    pub intrinsic: Option<crate::semantic::Intrinsic>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeChecks(pub u16);
impl RuntimeChecks {
    pub const TYPE: u16 = 1;
    pub const INITIALIZED: u16 = 2;
    pub const CALL: u16 = 4;
    pub const NO_WAIT: u16 = 8;
    pub const PATTERN: u16 = 16;
    pub const MUTABILITY: u16 = 32;
    pub const RECEIVER: u16 = 64;
    pub const DERIVED_SAFE: u16 = 128;
    pub const INIT_SAFE: u16 = 256;
    pub const PROPERTY: u16 = 512;
    pub const WRITE_EFFECT: u16 = 1024;
    pub const RECORD_FIELD: u16 = 2048;
}
#[derive(Debug, Clone)]
pub struct ControlTransfer {
    pub target: Option<NodeId>,
    /// Innermost to outermost. Throw uses dynamic handler selection at runtime.
    pub finally: Vec<NodeId>,
}
#[derive(Debug, Clone)]
pub struct Import {
    pub node: NodeId,
    pub module: String,
    pub version: String,
    pub names: Vec<(NameId, SymbolId)>,
}
#[derive(Debug, Clone, Default)]
pub struct AnalysisStats {
    pub nodes: usize,
    pub symbols: usize,
    pub captures: usize,
    pub work_units: usize,
    pub retained_bytes: usize,
    pub symbol_bytes: usize,
    pub capture_bytes: usize,
    pub imports: usize,
    pub signature_builds: usize,
    pub signature_parameters: usize,
}
/// Created only by successful analysis; future lowering must require this wrapper.
#[derive(Debug, Clone)]
pub struct AnalyzedModule {
    pub(crate) entry_context: crate::semantic::AnalysisContext,
    pub(crate) canonical_path: String,
    pub(crate) source_path: String,
    pub(crate) version: String,
    pub(crate) hir: Hir,
    pub(crate) facts: Vec<NodeFacts>,
    pub(crate) types: Vec<Type>,
    pub(crate) scopes: Vec<Scope>,
    pub(crate) symbols: Vec<Symbol>,
    pub(crate) functions: Vec<Function>,
    pub(crate) imports: Vec<Import>,
    pub(crate) exports: Vec<SymbolId>,
    pub(crate) stats: AnalysisStats,
    pub(crate) external: Vec<ExternalBinding>,
    pub(crate) source_types: crate::semantic::SourceTypes,
}
/// A local slot is stable within the fixed module content version, rather than
/// a process pointer or a globally meaningful arena index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalSymbolRef<'a> {
    pub module: &'a str,
    pub version: &'a str,
    pub slot: SymbolId,
}
impl AnalyzedModule {
    /// Source inference is separate from the annotated contracts used by lowering.
    pub fn source_types(&self) -> &crate::semantic::SourceTypes {
        &self.source_types
    }
    /// Render a semantic type using Aside names and unambiguous type syntax.
    pub fn display_type(&self, ty: TypeId) -> impl std::fmt::Display + '_ {
        TypeDisplay { module: self, ty }
    }
    pub fn entry_context(&self) -> crate::semantic::AnalysisContext {
        self.entry_context
    }
    pub fn symbol_reference(&self, slot: SymbolId) -> Option<LocalSymbolRef<'_>> {
        self.symbols.get(slot.0 as usize)?;
        Some(LocalSymbolRef {
            module: &self.canonical_path,
            version: &self.version,
            slot,
        })
    }

    pub fn canonical_path(&self) -> &str {
        &self.canonical_path
    }
    pub fn source_path(&self) -> &str {
        &self.source_path
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn hir(&self) -> &Hir {
        &self.hir
    }
    pub fn facts(&self) -> &[NodeFacts] {
        &self.facts
    }
    pub fn types(&self) -> &[Type] {
        &self.types
    }
    pub fn scopes(&self) -> &[Scope] {
        &self.scopes
    }
    pub fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }
    pub fn functions(&self) -> &[Function] {
        &self.functions
    }
    pub fn imports(&self) -> &[Import] {
        &self.imports
    }
    pub fn exports(&self) -> &[SymbolId] {
        &self.exports
    }
    pub fn stats(&self) -> &AnalysisStats {
        &self.stats
    }
    pub fn external_bindings(&self) -> &[ExternalBinding] {
        &self.external
    }
    /// Look up the exact owner in the binder's sorted external entries.
    /// Node entries precede symbol entries and each owner occurs once.
    pub(crate) fn external_binding(
        &self,
        symbol: Option<SymbolId>,
        node: Option<NodeId>,
    ) -> Option<&ExternalBinding> {
        self.external
            .binary_search_by_key(&(symbol, node), |binding| (binding.symbol, binding.node))
            .ok()
            .map(|index| &self.external[index])
    }
    /// Resolve a bound expression's canonical external identity, including
    /// immutable aliases and parenthesized references. This is metadata only;
    /// it does not replace evaluation of the expression or initialize aliases.
    pub fn canonical_reference(
        &self,
        mut node: NodeId,
    ) -> Option<&crate::semantic::SymbolReference> {
        while let (HirKind::Group, Form::Single { child }) =
            (self.hir.node(node).kind, self.hir.node(node).form)
        {
            node = child;
        }
        self.external_binding(None, Some(node))
            .or_else(|| {
                self.facts[node.0 as usize]
                    .symbol
                    .and_then(|symbol| self.external_binding(Some(symbol), None))
            })?
            .descriptor
            .reference
            .as_deref()
    }
}
struct TypeDisplay<'a> {
    module: &'a AnalyzedModule,
    ty: TypeId,
}
impl std::fmt::Display for TypeDisplay<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let display = |ty| TypeDisplay {
            module: self.module,
            ty,
        };
        match &self.module.types[self.ty.0 as usize] {
            Type::Any => f.write_str("any"),
            Type::Closure => f.write_str("closure"),
            Type::Null => f.write_str("null"),
            Type::Bool => f.write_str("bool"),
            Type::Int => f.write_str("int"),
            Type::Float => f.write_str("float"),
            Type::String => f.write_str("string"),
            Type::Array => f.write_str("array"),
            Type::Table => f.write_str("table"),
            Type::Future => f.write_str("Future"),
            Type::Generator => f.write_str("generator"),
            Type::Named(name) => f.write_str(self.module.hir.name(*name)),
            Type::StringLiteral(text) => write!(f, "{text:?}"),
            Type::Record(fields) => {
                f.write_str("{")?;
                for (i, &(name, ty)) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}: {}", self.module.hir.name(name), display(ty))?;
                }
                f.write_str("}")
            }
            Type::Tuple(types) => {
                f.write_str("(")?;
                for (i, &ty) in types.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}", display(ty))?;
                }
                if types.len() == 1 {
                    f.write_str(",")?;
                }
                f.write_str(")")
            }
            Type::Union(types) => {
                for (i, &ty) in types.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" | ")?;
                    }
                    if matches!(
                        self.module.types[ty.0 as usize],
                        Type::Callable(..) | Type::Function(_) | Type::BoundMethod(_)
                    ) {
                        write!(f, "({})", display(ty))?;
                    } else {
                        write!(f, "{}", display(ty))?;
                    }
                }
                Ok(())
            }
            Type::Callable(parameters, result) => {
                f.write_str("fn(")?;
                for (i, &ty) in parameters.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}", display(ty))?;
                }
                f.write_str(") -> ")?;
                if matches!(self.module.types[result.0 as usize], Type::Union(_)) {
                    write!(f, "({})", display(*result))
                } else {
                    write!(f, "{}", display(*result))
                }
            }
            Type::Function(id) | Type::BoundMethod(id) => {
                let function = &self.module.functions[id.0 as usize];
                let bound = matches!(self.module.types[self.ty.0 as usize], Type::BoundMethod(_));
                f.write_str("fn(")?;
                for (i, parameter) in function
                    .parameters
                    .iter()
                    .skip(usize::from(bound))
                    .enumerate()
                {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    if parameter.rest {
                        f.write_str("...")?;
                    }
                    write!(
                        f,
                        "{}",
                        display(self.module.symbols[parameter.symbol.0 as usize].ty)
                    )?;
                }
                f.write_str(") -> ")?;
                if matches!(
                    self.module.types[function.return_type.0 as usize],
                    Type::Union(_)
                ) {
                    write!(f, "({})", display(function.return_type))
                } else {
                    write!(f, "{}", display(function.return_type))
                }
            }
            Type::Class(node) | Type::Instance(node) => match self.module.hir.node(*node).form {
                Form::Class {
                    name: Some(name), ..
                } => f.write_str(self.module.hir.text(name.span)),
                _ => f.write_str("class"),
            },
        }
    }
}

/// Full resolver signature survives analysis, including named-call semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalBindingKind {
    /// An executable module cell or registered native reference.
    Reference,
    /// A declared type used only by source annotations and runtime type checks.
    Type,
    /// An ordinary value or object's member contract. Execution retains its
    /// source binding or receiver instead of substituting a linked reference.
    Metadata,
}
#[derive(Debug, Clone)]
pub struct ExternalBinding {
    pub kind: ExternalBindingKind,
    pub symbol: Option<SymbolId>,
    pub node: Option<NodeId>,
    pub descriptor: Arc<crate::semantic::ExternalSymbol>,
}

/// Grammar roles resolved once during projection. Sequence items use the
/// existing packed children edges; no second per-node child allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKey {
    Name(HirToken),
    Expression(NodeId),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    Sequence,
    Block {
        tail: Option<NodeId>,
    },
    Binding {
        pattern: NodeId,
        annotation: Option<NodeId>,
        initializer: Option<NodeId>,
        mutable: bool,
    },
    Function {
        name: Option<HirToken>,
        parameters: Option<NodeId>,
        result: Option<NodeId>,
        body: NodeId,
        kind: FunctionKind,
    },
    Class {
        name: Option<HirToken>,
        base: Option<NodeId>,
    },
    Parameter {
        name: HirToken,
        annotation: Option<NodeId>,
        default: Option<NodeId>,
        mutable: bool,
        rest: bool,
    },
    Import {
        path: NodeId,
    },
    ImportPath {
        alias: Option<HirToken>,
        group: Option<NodeId>,
        wildcard: bool,
    },
    TypeName,
    FunctionType {
        result: NodeId,
    },
    Single {
        child: NodeId,
    },
    Statement {
        expression: NodeId,
        terminated: bool,
    },
    Transfer {
        value: Option<NodeId>,
        label: Option<HirToken>,
    },
    Name {
        name: HirToken,
    },
    Wildcard,
    Literal {
        token: HirToken,
    },
    LiteralPattern {
        value: NodeId,
        negative: bool,
    },
    Field {
        key: FieldKey,
        value: NodeId,
    },
    Spread {
        value: NodeId,
    },
    Unary {
        operator: HirToken,
        value: NodeId,
    },
    Binary {
        left: NodeId,
        operator: HirToken,
        right: NodeId,
    },
    Member {
        object: NodeId,
        key: HirToken,
        optional: bool,
        chain: bool,
    },
    Index {
        object: NodeId,
        index: NodeId,
        optional: bool,
        chain: bool,
    },
    Call {
        callee: NodeId,
        arguments: Option<NodeId>,
        children: Option<NodeId>,
        optional: bool,
        chain: bool,
    },
    Argument {
        name: Option<HirToken>,
        value: NodeId,
    },
    Condition {
        pattern: Option<NodeId>,
        condition: NodeId,
        body: NodeId,
        otherwise: Option<NodeId>,
    },
    For {
        pattern: NodeId,
        source: NodeId,
        body: NodeId,
        asynchronous: bool,
    },
    Label {
        name: HirToken,
        body: NodeId,
    },
    Match {
        value: NodeId,
    },
    MatchArm {
        pattern: NodeId,
        guard: Option<NodeId>,
        body: NodeId,
    },
    Try {
        body: NodeId,
        catch: Option<NodeId>,
        finally: Option<NodeId>,
    },
    Catch {
        name: HirToken,
        body: NodeId,
    },
    PatternField {
        key: HirToken,
        pattern: Option<NodeId>,
        default: Option<NodeId>,
    },
    Or {
        left: NodeId,
        right: NodeId,
    },
    TypePattern {
        pattern: NodeId,
        annotation: NodeId,
    },
    Modified {
        declaration: NodeId,
        public: bool,
        static_member: bool,
    },
}
