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
    Module, Block, Let, Function, Class, Import, ImportPath, ImportGroup,
    Parameters, Parameter, NamedType, TupleType, UnionType, FunctionType, GroupType,
    ExpressionStatement, Return, Break, Continue, Throw, Yield, Name, Literal,
    String, Interpolation, Tuple, Group, Array, Object, ObjectField, Spread,
    Prefix, Binary, Range, Assignment, Member, Index, Call, Arguments, Argument,
    NamedArgument, Children, Closure, If, While, For, Loop, Label, Match, MatchArm,
    Try, Catch, Finally, NamePattern, WildcardPattern, LiteralPattern,
    TuplePattern, GroupPattern, ArrayPattern, ObjectPattern, PatternField,
    RestPattern, OrPattern, TypePattern, ModifiedDeclaration,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HirToken {
    pub kind: SyntaxKind,
    pub span: TextRange,
    /// Identifiers are interned once per module. Other text is a source slice.
    pub name: Option<NameId>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirElement { Node(NodeId), Token(HirToken) }
#[derive(Debug, Clone)]
pub struct HirNode {
    pub kind: HirKind,
    pub span: TextRange,
    pub parent: Option<NodeId>,
    pub elements: Range<u32>,
}
#[derive(Debug, Clone)]
pub struct Hir {
    pub source: Arc<str>,
    pub nodes: Vec<HirNode>,
    pub elements: Vec<HirElement>,
    pub names: Vec<String>,
}
impl Hir {
    pub fn node(&self, id: NodeId) -> &HirNode { &self.nodes[id.0 as usize] }
    pub fn elements(&self, id: NodeId) -> &[HirElement] {
        let r = &self.node(id).elements;
        &self.elements[r.start as usize..r.end as usize]
    }
    pub fn children(&self, id: NodeId) -> impl Iterator<Item=NodeId> + '_ {
        self.elements(id).iter().filter_map(|e| match e { HirElement::Node(n) => Some(*n), _ => None })
    }
    pub fn tokens(&self, id: NodeId) -> impl Iterator<Item=HirToken> + '_ {
        self.elements(id).iter().filter_map(|e| match e { HirElement::Token(t) => Some(*t), _ => None })
    }
    pub fn name(&self, id: NameId) -> &str { &self.names[id.0 as usize] }
    pub fn text(&self, span: TextRange) -> &str { &self.source[crate::byte_range(span)] }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Any, Null, Bool, Int, Float, String, Array, Table, Future, Generator,
    Tuple(Vec<TypeId>), Union(Vec<TypeId>), Callable(Vec<TypeId>, TypeId),
    Function(FunctionId), Class(NodeId), Instance(NodeId), Named(NameId),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind { Local, Parameter, Function, Class, Import, Host, Catch, Receiver, Super }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage { Local, Module, ValueCapture, SharedCell }
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: NameId,
    pub declaration: TextRange,
    pub scope: ScopeId,
    pub owner: Option<FunctionId>,
    pub kind: SymbolKind,
    pub mutable: bool,
    pub ty: TypeId,
    pub storage: Storage,
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
pub enum FunctionKind { Plain, Async, Generator }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Receiver { None, ExplicitSelf, Static }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode { Value, Cell }
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
#[derive(Debug, Clone)]
pub struct Function {
    pub node: NodeId,
    pub parent: Option<FunctionId>,
    pub kind: FunctionKind,
    pub receiver: Receiver,
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
}
/// Created only by successful analysis; future lowering must require this wrapper.
#[derive(Debug, Clone)]
pub struct AnalyzedModule {
    pub(crate) hir: Hir,
    pub(crate) facts: Vec<NodeFacts>,
    pub(crate) types: Vec<Type>,
    pub(crate) scopes: Vec<Scope>,
    pub(crate) symbols: Vec<Symbol>,
    pub(crate) functions: Vec<Function>,
    pub(crate) imports: Vec<Import>,
    pub(crate) exports: Vec<SymbolId>,
    pub(crate) stats: AnalysisStats,
}
impl AnalyzedModule {
    pub fn hir(&self) -> &Hir { &self.hir }
    pub fn facts(&self) -> &[NodeFacts] { &self.facts }
    pub fn types(&self) -> &[Type] { &self.types }
    pub fn scopes(&self) -> &[Scope] { &self.scopes }
    pub fn symbols(&self) -> &[Symbol] { &self.symbols }
    pub fn functions(&self) -> &[Function] { &self.functions }
    pub fn imports(&self) -> &[Import] { &self.imports }
    pub fn exports(&self) -> &[SymbolId] { &self.exports }
    pub fn stats(&self) -> &AnalysisStats { &self.stats }
}
