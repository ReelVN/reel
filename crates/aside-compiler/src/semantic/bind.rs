use super::*;
mod arguments;
mod refinement;
mod results;
use crate::hir::*;
use crate::{DiagnosticCode as D, RelatedRange, SyntaxKind as K, TextRange};
use hashbrown::HashTable;
use smallvec::{SmallVec, smallvec};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher, Hash};
use std::mem::size_of;
use std::sync::Arc;

fn intern_hash(value: &(impl Hash + ?Sized)) -> u64 {
    BuildHasherDefault::<DefaultHasher>::default().hash_one(value)
}

const ANY: TypeId = TypeId(0);
const NULL: TypeId = TypeId(1);
const BOOL: TypeId = TypeId(2);
const INT: TypeId = TypeId(3);
const FLOAT: TypeId = TypeId(4);
const STRING: TypeId = TypeId(5);
const ARRAY: TypeId = TypeId(6);
const TABLE: TypeId = TypeId(7);
const FUTURE: TypeId = TypeId(8);
const GENERATOR: TypeId = TypeId(9);

#[derive(Clone, Default)]
struct Context {
    function: Option<FunctionId>,
    default: bool,
    // Source return scope is independent of synthesized function owners.
    default_return_boundary: Option<NodeId>,
    no_wait: bool,
    catch: bool,
    finally: Vec<NodeId>,
    // Loop target + finally depth when entered.
    loops: Vec<(NodeId, Option<NameId>, usize)>,
    // Each finally disallows transfers outside its own boundary.
    cleanup_boundary: Option<usize>,
    generator: bool,
    iteration: bool,
    conditional: bool,
    derived: bool,
    init: bool,
}
impl super::patterns::PatternTypes for Binder<'_> {
    type Ty = TypeId;
    fn hir(&self) -> &Hir {
        &self.out.hir
    }
    fn unknown(&self) -> TypeId {
        ANY
    }
    fn shape(&self, ty: &TypeId) -> super::patterns::Shape<TypeId> {
        use super::patterns::Shape;
        match &self.out.types[ty.0 as usize] {
            Type::Any | Type::Union(_) | Type::Named(_) | Type::Instance(_) => Shape::Unknown,
            Type::Tuple(elements) => Shape::Tuple(elements.clone()),
            Type::Array => Shape::Array,
            Type::Table => Shape::Table,
            _ => Shape::Other,
        }
    }
    fn tuple(&mut self, elements: Vec<TypeId>) -> TypeId {
        self.intern_type(Type::Tuple(elements))
    }
    fn array(&self) -> TypeId {
        ARRAY
    }
    fn table(&self) -> TypeId {
        TABLE
    }
    fn annotation(&mut self, node: NodeId) -> TypeId {
        self.ty(node)
    }
    fn compatible(&self, actual: &TypeId, expected: &TypeId) -> bool {
        self.compatible(*actual, *expected)
    }
    fn charge(&mut self, node: NodeId) -> bool {
        self.budget(node)
    }
}
#[derive(Clone)]
struct CallSite {
    node: NodeId,
    caller: Option<FunctionId>,
    callee: Option<FunctionId>,
    bound: bool,
    effect: CallEffect,
    no_wait: bool,
    named_callee: Option<NameId>,
}
#[derive(Clone)]
struct PatternName {
    node: NodeId,
    name: NameId,
    span: TextRange,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum PatternUse {
    Existing,
    Declaration,
    Branch,
    Assignment,
}
struct Declaration {
    name: NameId,
    span: TextRange,
    kind: SymbolKind,
    mutable: bool,
    ty: TypeId,
    owner: Option<FunctionId>,
    iteration: bool,
}
struct PendingRequirement {
    node: NodeId,
    actual: TypeId,
    expected: TypeId,
    label: String,
}
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum SignatureKey {
    Function(FunctionId, bool),
    External(usize),
}
struct CheckedParameter {
    name: NameId,
    ty: TypeId,
    required: bool,
    rest: bool,
}
struct CheckedSignature {
    parameters: Vec<CheckedParameter>,
    named: HashMap<NameId, usize>,
    rest: Option<usize>,
    required: Vec<usize>,
}
#[derive(Clone)]
struct ExternalDescriptor {
    symbol: Arc<ExternalSymbol>,
    kind: ExternalBindingKind,
}
impl ExternalDescriptor {
    fn reference(symbol: ExternalSymbol) -> Self {
        let kind = if symbol.type_only {
            ExternalBindingKind::Type
        } else {
            ExternalBindingKind::Reference
        };
        Self {
            symbol: Arc::new(symbol),
            kind,
        }
    }
    fn metadata(symbol: ExternalSymbol) -> Self {
        Self {
            symbol: Arc::new(symbol),
            kind: ExternalBindingKind::Metadata,
        }
    }
}
impl std::ops::Deref for ExternalDescriptor {
    type Target = ExternalSymbol;
    fn deref(&self) -> &ExternalSymbol {
        &self.symbol
    }
}
struct Binder<'a> {
    out: AnalyzedModule,
    options: &'a AnalysisOptions,
    resolver: &'a mut dyn ModuleResolver,
    diagnostics: Vec<Diagnostic>,
    exhausted: bool,
    scope: ScopeId,
    root_scope: ScopeId,
    maps: Vec<HashMap<NameId, SymbolId>>,
    names: HashTable<NameId>,
    types: HashTable<TypeId>,
    initialized: Vec<bool>,
    annotations: HashSet<SymbolId>,
    refinements: HashMap<SymbolId, TypeId>,
    refinement_undo: Vec<(SymbolId, Option<TypeId>)>,
    fn_nodes: HashMap<NodeId, FunctionId>,
    capture_maps: Vec<HashMap<SymbolId, usize>>,
    capture_users: Vec<Vec<(FunctionId, usize)>>,
    external: HashMap<SymbolId, ExternalDescriptor>,
    external_nodes: HashMap<NodeId, ExternalDescriptor>,
    member_write_types: HashMap<NodeId, Option<TypeId>>,
    calls: Vec<CallSite>,
    analyzed_functions: HashSet<FunctionId>,
    signature_ready: HashSet<FunctionId>,
    default_boundaries: Vec<(FunctionId, SymbolId)>,
    pending_requirements: Vec<PendingRequirement>,
    signatures: HashMap<SignatureKey, Arc<CheckedSignature>>,
    empty_signature: Option<Arc<CheckedSignature>>,
    callable_types: HashMap<NameId, Option<ExternalDescriptor>>,
    await_types: HashMap<NameId, Option<TypeId>>,
    class: Option<(NodeId, bool)>,
    class_members: HashMap<NodeId, HashMap<(NameId, bool), NodeId>>,
    prepared_classes: HashSet<NodeId>,
    class_bases: HashMap<NodeId, NodeId>,
    derived_functions: HashSet<NodeId>,
    fresh: HashSet<SymbolId>,
    work: usize,
    capture_count: usize,
}
pub(super) fn analyze(
    hir: Hir,
    options: &AnalysisOptions,
    resolver: &mut dyn ModuleResolver,
) -> Analysis {
    let result = analyze_editor(hir, options, resolver);
    Analysis {
        module: if result.diagnostics.is_empty() && !result.exhausted {
            result.module
        } else {
            None
        },
        diagnostics: result.diagnostics,
        exhausted: result.exhausted,
        stats: result.stats,
    }
}

pub(super) fn analyze_editor(
    hir: Hir,
    options: &AnalysisOptions,
    resolver: &mut dyn ModuleResolver,
) -> crate::editor::EditorAnalysis {
    // Admit the seeded name index before allocating binding metadata.
    let admitted = hir.names.len().saturating_add(1);
    if admitted > options.limits.max_work {
        return crate::editor::EditorAnalysis {
            module: None,
            diagnostics: vec![Diagnostic::error(
                D::BudgetExceeded,
                hir.node(NodeId(0)).span,
                "semantic work budget exceeded before name indexing",
            )],
            exhausted: true,
            stats: AnalysisStats {
                nodes: hir.nodes.len(),
                work_units: admitted,
                ..Default::default()
            },
        };
    }
    let mut names = HashTable::with_capacity(hir.names.len());
    for (i, name) in hir.names.iter().enumerate() {
        let hash = intern_hash(name);
        if let Some(id) = names.find_mut(hash, |id: &NameId| hir.names[id.0 as usize] == *name) {
            *id = NameId(i as u32);
        } else {
            names.insert_unique(hash, NameId(i as u32), |id| {
                intern_hash(&hir.names[id.0 as usize])
            });
        }
    }
    let types = vec![
        Type::Any,
        Type::Null,
        Type::Bool,
        Type::Int,
        Type::Float,
        Type::String,
        Type::Array,
        Type::Table,
        Type::Future,
        Type::Generator,
    ];
    let mut type_map = HashTable::with_capacity(types.len());
    for (i, ty) in types.iter().enumerate() {
        type_map.insert_unique(intern_hash(ty), TypeId(i as u32), |id| {
            intern_hash(&types[id.0 as usize])
        });
    }
    let span = hir.node(NodeId(0)).span;
    let facts = vec![
        NodeFacts {
            ty: ANY,
            symbol: None,
            function: None,
            scope: None,
            checks: RuntimeChecks::default(),
            control: None,
            intrinsic: None
        };
        hir.nodes.len()
    ];
    let mut b = Binder {
        out: AnalyzedModule {
            entry_context: options.context,
            canonical_path: options.module.clone(),
            source_path: options
                .source_path
                .as_ref()
                .unwrap_or(&options.module)
                .clone(),
            version: options.version.clone(),
            hir,
            facts,
            types,
            scopes: Vec::new(),
            symbols: Vec::new(),
            functions: Vec::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            stats: AnalysisStats::default(),
            external: Vec::new(),
            source_types: SourceTypes::default(),
        },
        options,
        resolver,
        diagnostics: Vec::new(),
        exhausted: false,
        scope: ScopeId(0),
        root_scope: ScopeId(0),
        maps: Vec::new(),
        names,
        types: type_map,
        initialized: Vec::new(),
        annotations: HashSet::new(),
        refinements: HashMap::new(),
        refinement_undo: Vec::new(),
        fn_nodes: HashMap::new(),
        capture_maps: Vec::new(),
        capture_users: Vec::new(),
        external: HashMap::new(),
        external_nodes: HashMap::new(),
        member_write_types: HashMap::new(),
        calls: Vec::new(),
        analyzed_functions: HashSet::new(),
        signature_ready: HashSet::new(),
        default_boundaries: Vec::new(),
        pending_requirements: Vec::new(),
        signatures: HashMap::new(),
        empty_signature: None,
        callable_types: HashMap::new(),
        await_types: HashMap::new(),
        class: None,
        class_members: HashMap::new(),
        prepared_classes: HashSet::new(),
        class_bases: HashMap::new(),
        derived_functions: HashSet::new(),
        fresh: HashSet::new(),
        work: admitted,
        capture_count: 0,
    };
    b.new_scope(None, None, span);
    for host in &options.hosts {
        if !b.budget(NodeId(0)) {
            break;
        }
        let name = b.intern_name(&host.name);
        let ty = b.hint(&host.ty);
        if let Some(symbol) = b.declare(Declaration {
            name,
            span,
            kind: SymbolKind::Host,
            mutable: false,
            ty,
            owner: None,
            iteration: false,
        }) {
            b.initialized[symbol.0 as usize] = true;
            b.external
                .insert(symbol, ExternalDescriptor::reference(host.clone()));
        }
    }
    let mut ctx = Context {
        no_wait: matches!(
            options.context,
            AnalysisContext::Module | AnalysisContext::NoWait | AnalysisContext::Generator
        ),
        generator: options.context == AnalysisContext::Generator,
        init: options.context == AnalysisContext::Module,
        ..Default::default()
    };
    if matches!(
        options.context,
        AnalysisContext::Function | AnalysisContext::Generator
    ) {
        // Fragments are explicitly running author functions without fabricated source offsets.
        let id = b.make_function(NodeId(0), &ctx, Receiver::None);
        ctx.function = Some(id);
        b.out.functions[id.0 as usize].declared_context = DeclaredContext {
            no_wait: ctx.no_wait,
            init: ctx.init,
            derived: ctx.derived,
        };
        if options.context == AnalysisContext::Generator {
            b.out.functions[id.0 as usize].kind = FunctionKind::Generator;
        }
    }
    b.scope = b.new_scope(Some(ScopeId(0)), ctx.function, span);
    b.root_scope = b.scope;
    for binding in &options.enclosing {
        if !b.budget(NodeId(0)) {
            break;
        }
        let name = b.intern_name(&binding.symbol.name);
        let ty = b.external_type(&binding.symbol);
        if let Some(symbol) = b.declare_binding(
            Declaration {
                name,
                span: TextRange::empty(span.start()),
                kind: SymbolKind::Local,
                mutable: binding.mutable,
                ty,
                owner: ctx.function,
                iteration: false,
            },
            Initialization::External,
        ) {
            b.initialized[symbol.0 as usize] = true;
            b.external.insert(
                symbol,
                ExternalDescriptor::reference(binding.symbol.clone()),
            );
        }
    }
    b.block(NodeId(0), &ctx, false);
    for requirement in std::mem::take(&mut b.pending_requirements) {
        b.require(
            requirement.node,
            requirement.actual,
            requirement.expected,
            &requirement.label,
        );
    }
    b.effects();
    b.check_results();
    b.account();
    let stats = b.out.stats.clone();
    crate::editor::EditorAnalysis {
        stats,
        module: Some(b.out),
        diagnostics: b.diagnostics,
        exhausted: b.exhausted,
    }
}
mod control_flow;
mod declarations;
mod expressions;
mod functions;
mod members;
mod types;

impl Binder<'_> {
    fn resolve(&mut self, request: &ImportRequest) -> Result<ResolvedImport, ResolveError> {
        self.resolver
            .resolve(request)
            .map(ResolvedImport::retain_references)
    }
    fn kind(&self, n: NodeId) -> HirKind {
        self.out.hir.node(n).kind
    }
    fn span(&self, n: NodeId) -> TextRange {
        self.out.hir.node(n).span
    }
    fn children(&self, n: NodeId) -> SmallVec<[NodeId; 4]> {
        self.out.hir.children(n).collect()
    }
    fn tokens(&self, n: NodeId) -> SmallVec<[HirToken; 4]> {
        self.out.hir.tokens(n).collect()
    }
    fn token(&self, n: NodeId, k: K) -> Option<HirToken> {
        self.out.hir.tokens(n).find(|t| t.kind == k)
    }
    fn name(&self, n: NodeId) -> Option<HirToken> {
        self.token(n, K::Ident)
    }
    fn check(&mut self, n: NodeId, flag: u16) {
        self.out.facts[n.0 as usize].checks.0 |= flag;
    }
    fn ty(&self, n: NodeId) -> TypeId {
        self.out.facts[n.0 as usize].ty
    }
    fn set_ty(&mut self, n: NodeId, t: TypeId) -> TypeId {
        self.out.facts[n.0 as usize].ty = t;
        t
    }
    fn error(&mut self, code: D, span: TextRange, message: impl Into<String>) {
        if self.diagnostics.len() < self.options.limits.max_diagnostics.max(1) {
            self.diagnostics
                .push(Diagnostic::error(code, span, message));
        }
    }
    fn budget(&mut self, n: NodeId) -> bool {
        self.charge_work(n, 1)
    }
    fn charge_work(&mut self, n: NodeId, units: usize) -> bool {
        self.charge_at(self.span(n), units)
    }
    fn charge_at(&mut self, span: TextRange, units: usize) -> bool {
        if self.exhausted {
            return false;
        }
        self.work = self.work.saturating_add(units);
        if self.work > self.options.limits.max_work {
            self.exhausted = true;
            self.error(D::BudgetExceeded, span, "semantic work budget exceeded");
            return false;
        }
        true
    }
    fn intern_name(&mut self, s: &str) -> NameId {
        let hash = intern_hash(s);
        if let Some(&id) = self
            .names
            .find(hash, |id| self.out.hir.names[id.0 as usize] == s)
        {
            return id;
        }
        let id = NameId(self.out.hir.names.len() as u32);
        self.out.hir.names.push(s.to_owned());
        self.names.insert_unique(hash, id, |id| {
            intern_hash(&self.out.hir.names[id.0 as usize])
        });
        id
    }
    fn intern_type(&mut self, t: Type) -> TypeId {
        let hash = intern_hash(&t);
        if let Some(&id) = self
            .types
            .find(hash, |id| self.out.types[id.0 as usize] == t)
        {
            return id;
        }
        let id = TypeId(self.out.types.len() as u32);
        self.out.types.push(t);
        self.types
            .insert_unique(hash, id, |id| intern_hash(&self.out.types[id.0 as usize]));
        id
    }
    fn external_type(&mut self, symbol: &ExternalSymbol) -> TypeId {
        if matches!(symbol.ty, TypeHint::Named(_))
            && let Some(reference) = &symbol.reference
            && let Some(export) = &reference.export
        {
            let name = self.intern_name(&format!("{}::{export}", reference.module));
            return self.intern_type(Type::Named(name));
        }
        self.hint(&symbol.ty)
    }
    fn hint(&mut self, t: &TypeHint) -> TypeId {
        match t {
            TypeHint::Any => ANY,
            TypeHint::Closure => self.intern_type(Type::Closure),
            TypeHint::Null => NULL,
            TypeHint::Bool => BOOL,
            TypeHint::Int => INT,
            TypeHint::Float => FLOAT,
            TypeHint::String => STRING,
            TypeHint::Array => ARRAY,
            TypeHint::Table => TABLE,
            TypeHint::Future => FUTURE,
            TypeHint::Generator => GENERATOR,
            TypeHint::Tuple(ts) => {
                let v = ts.iter().map(|t| self.hint(t)).collect();
                self.intern_type(Type::Tuple(v))
            }
            TypeHint::Union(ts) => {
                let v = ts.iter().map(|t| self.hint(t)).collect();
                self.intern_type(Type::Union(v))
            }
            TypeHint::Callable(ts, r) => {
                let v = ts.iter().map(|t| self.hint(t)).collect();
                let r = self.hint(r);
                self.intern_type(Type::Callable(v, r))
            }
            TypeHint::Named(s) => {
                let n = self.intern_name(s);
                self.intern_type(Type::Named(n))
            }
        }
    }
    fn new_scope(
        &mut self,
        parent: Option<ScopeId>,
        owner: Option<FunctionId>,
        span: TextRange,
    ) -> ScopeId {
        let id = ScopeId(self.out.scopes.len() as u32);
        self.out.scopes.push(Scope {
            parent,
            owner,
            span,
            symbols: Vec::new(),
        });
        self.maps.push(HashMap::new());
        id
    }
    fn declare(&mut self, declaration: Declaration) -> Option<SymbolId> {
        let initialization = match declaration.kind {
            SymbolKind::Function => Initialization::ScopeEntry,
            SymbolKind::Parameter | SymbolKind::Receiver | SymbolKind::Super => {
                Initialization::Parameter
            }
            SymbolKind::Host | SymbolKind::Import => Initialization::External,
            _ => Initialization::Declaration,
        };
        self.declare_binding(declaration, initialization)
    }
    fn declare_binding(
        &mut self,
        declaration: Declaration,
        initialization: Initialization,
    ) -> Option<SymbolId> {
        let Declaration {
            name,
            span,
            kind,
            mutable,
            ty,
            owner,
            iteration,
        } = declaration;
        if !self.charge_at(span, 1) {
            return None;
        }
        if let Some(&old) = self.maps[self.scope.0 as usize].get(&name) {
            self.error(
                D::DuplicateBinding,
                span,
                format!("duplicate binding {}", self.out.hir.name(name)),
            );
            if let Some(d) = self.diagnostics.last_mut() {
                d.related.push(RelatedRange {
                    range: self.out.symbols[old.0 as usize].declaration,
                    message: "previous declaration".into(),
                });
            }
            return None;
        }
        if self.out.symbols.len() >= self.options.limits.max_symbols {
            if !self.exhausted {
                self.error(D::BudgetExceeded, span, "symbol budget exceeded");
            }
            self.exhausted = true;
            return None;
        }
        let id = SymbolId(self.out.symbols.len() as u32);
        let origin = if matches!(kind, SymbolKind::Host | SymbolKind::Import) {
            BindingOrigin::External
        } else if let Some(owner) = owner {
            BindingOrigin::FunctionLocal(owner)
        } else if self.options.context == AnalysisContext::Module
            && self.scope == self.root_scope
            && initialization != Initialization::External
            && matches!(
                kind,
                SymbolKind::Local | SymbolKind::Function | SymbolKind::Class
            )
        {
            BindingOrigin::ModuleGlobal
        } else {
            BindingOrigin::EntryLocal
        };
        self.out.symbols.push(Symbol {
            name,
            declaration: span,
            scope: self.scope,
            origin,
            owner: origin.function(),
            kind,
            mutable,
            ty,
            storage: if origin == BindingOrigin::ModuleGlobal {
                Storage::Module
            } else {
                Storage::Local
            },
            per_iteration: iteration,
            initialization,
        });
        self.initialized.push(false);
        self.capture_users.push(Vec::new());
        self.maps[self.scope.0 as usize].insert(name, id);
        self.out.scopes[self.scope.0 as usize].symbols.push(id);
        Some(id)
    }
    fn nearest_symbol(&self, name: NameId) -> Option<SymbolId> {
        let mut scope = Some(self.scope);
        while let Some(id) = scope {
            if let Some(&symbol) = self.maps[id.0 as usize].get(&name) {
                return Some(symbol);
            }
            scope = self.out.scopes[id.0 as usize].parent;
        }
        None
    }
    fn lookup(&self, name: NameId) -> Option<SymbolId> {
        let mut s = Some(self.scope);
        while let Some(id) = s {
            if let Some(&symbol) = self.maps[id.0 as usize].get(&name) {
                let binding = &self.out.symbols[symbol.0 as usize];
                let hidden = self.default_boundaries.iter().any(|&(owner, boundary)| {
                    binding.origin.function() == Some(owner)
                        && (binding.kind == SymbolKind::Super
                            || (matches!(
                                binding.kind,
                                SymbolKind::Parameter | SymbolKind::Receiver
                            ) && symbol.0 >= boundary.0))
                });
                if !hidden {
                    return Some(symbol);
                }
            }
            s = self.out.scopes[id.0 as usize].parent;
        }
        None
    }
    fn unwrap(&self, n: NodeId) -> NodeId {
        if self.kind(n) == HirKind::ModifiedDeclaration {
            self.out.hir.children(n).next().unwrap_or(n)
        } else {
            n
        }
    }
    fn modifier(&self, n: NodeId, k: K) -> bool {
        self.token(n, k).is_some()
    }
    fn predeclare(&mut self, n: NodeId, ctx: &Context) {
        let d = self.unwrap(n);
        if matches!(
            self.kind(d),
            HirKind::Function | HirKind::Let | HirKind::Class
        ) && !self.budget(d)
        {
            return;
        }
        match self.kind(d) {
            HirKind::Function => {
                let receiver = Receiver::None;
                let f = self.make_function(d, ctx, receiver);
                let ty = self.intern_type(Type::Function(f));
                if let Some(t) = self.name(d)
                    && let Some(s) = self.declare(Declaration {
                        name: t.name.unwrap(),
                        span: t.span,
                        kind: SymbolKind::Function,
                        mutable: false,
                        ty,
                        owner: ctx.function,
                        iteration: ctx.iteration,
                    })
                {
                    self.initialized[s.0 as usize] = true;
                    self.out.facts[d.0 as usize].symbol = Some(s);
                    if self.modifier(n, K::Pub) {
                        self.out.exports.push(s);
                    }
                }
            }
            HirKind::Let => {
                if let Some(p) = self
                    .children(d)
                    .into_iter()
                    .find(|&p| is_pattern(self.kind(p)))
                {
                    for name in self.pattern_names(p) {
                        if let Some(s) = self.declare(Declaration {
                            name: name.name,
                            span: name.span,
                            kind: SymbolKind::Local,
                            mutable: self.modifier(d, K::Mut),
                            ty: ANY,
                            owner: ctx.function,
                            iteration: ctx.iteration,
                        }) {
                            self.out.facts[name.node.0 as usize].symbol = Some(s);
                            if self.modifier(n, K::Pub) {
                                self.out.exports.push(s);
                            }
                        }
                    }
                }
            }
            HirKind::Class => {
                if let Some(t) = self.name(d) {
                    let ty = self.intern_type(Type::Class(d));
                    if let Some(s) = self.declare(Declaration {
                        name: t.name.unwrap(),
                        span: t.span,
                        kind: SymbolKind::Class,
                        mutable: false,
                        ty,
                        owner: ctx.function,
                        iteration: ctx.iteration,
                    }) {
                        self.out.facts[d.0 as usize].symbol = Some(s);
                        if self.modifier(n, K::Pub) {
                            self.out.exports.push(s);
                        }
                    }
                }
            }
            _ => (),
        }
    }
    fn block(&mut self, n: NodeId, ctx: &Context, new_scope: bool) -> TypeId {
        if !self.budget(n) {
            return ANY;
        }
        let previous = self.scope;
        if new_scope {
            self.scope = self.new_scope(Some(previous), ctx.function, self.span(n));
        }
        self.out.facts[n.0 as usize].scope = Some(self.scope);
        let children = self.children(n);
        for &child in &children {
            if self.exhausted {
                break;
            }
            self.predeclare(child, ctx);
        }
        // Imports are linked before visiting initializers, independent of textual order.
        for &child in &children {
            if self.exhausted {
                break;
            }
            let d = self.unwrap(child);
            if self.kind(d) == HirKind::Import {
                self.import(d, self.modifier(child, K::Pub), ctx);
            }
        }
        // Class shape is needed when a forward function body uses an instance.
        // Method and field initializer bodies are still analyzed at the declaration.
        for &child in &children {
            let declaration = self.unwrap(child);
            if self.kind(declaration) == HirKind::Class {
                self.prepare_class(declaration, ctx);
            }
        }
        // Prepare every declared contract before visiting any consumer default/body.
        for &child in &children {
            if self.exhausted {
                break;
            }
            let d = self.unwrap(child);
            if self.kind(d) == HirKind::Function {
                self.prepare_function(d, ctx, Receiver::None);
            }
        }
        // Signatures and definition environments are available at block entry,
        // including forward fn references. Ordinary lets remain uninitialized.
        for &child in &children {
            if self.exhausted {
                break;
            }
            let d = self.unwrap(child);
            if self.kind(d) == HirKind::Function {
                self.function(d, ctx, Receiver::None);
            }
        }
        let mut result = NULL;
        for &child in &children {
            if self.exhausted {
                break;
            }
            if self.kind(self.unwrap(child)) != HirKind::Import {
                result = self.visit(child, ctx);
            }
        }
        if let Some(&last) = children.last()
            && (!matches!(
                self.kind(last),
                HirKind::ExpressionStatement | HirKind::Block
            ) || (self.kind(last) == HirKind::ExpressionStatement
                && self.token(last, K::Semi).is_some()))
        {
            result = NULL;
        }
        self.scope = previous;
        self.set_ty(n, result)
    }

    fn read(&mut self, n: NodeId, token: HirToken, ctx: &Context, write: bool) -> TypeId {
        let name = token.name.unwrap();
        if ctx.default
            && self.nearest_symbol(name).is_some_and(|symbol| {
                let binding = &self.out.symbols[symbol.0 as usize];
                self.default_boundaries.iter().any(|&(owner, boundary)| {
                    ctx.function == Some(owner)
                        && binding.origin.function() == Some(owner)
                        && matches!(binding.kind, SymbolKind::Parameter | SymbolKind::Receiver)
                        && symbol.0 >= boundary.0
                })
            })
        {
            self.error(
                D::InvalidDefault,
                token.span,
                "default cannot reference its own or a later parameter",
            );
            return self.set_ty(n, ANY);
        }
        let Some(s) = self.lookup(name) else {
            self.error(
                D::UnknownName,
                token.span,
                format!("unknown name {}", self.out.hir.name(name)),
            );
            return self.set_ty(n, ANY);
        };
        self.out.facts[n.0 as usize].symbol = Some(s);
        if self
            .external
            .get(&s)
            .is_some_and(|descriptor| descriptor.type_only)
        {
            self.error(
                D::InvalidType,
                token.span,
                "a type declaration cannot be read as a value",
            );
            if let Some(descriptor) = self.external.get(&s) {
                self.external_nodes.insert(n, descriptor.clone());
            }
            return self.set_ty(n, ANY);
        }
        let symbol = self.out.symbols[s.0 as usize].clone();
        if symbol.kind == SymbolKind::Import {
            self.check(n, RuntimeChecks::INITIALIZED);
        }
        if !self.initialized[s.0 as usize] {
            if symbol.origin.same_frame(ctx.function) {
                self.error(
                    D::UninitializedBinding,
                    token.span,
                    "binding is not initialized",
                );
            } else {
                self.check(n, RuntimeChecks::INITIALIZED);
            }
        }
        if write && ctx.derived && !symbol.origin.same_frame(ctx.function) && !ctx.conditional {
            self.error(
                D::InvalidEffect,
                token.span,
                "derived evaluation cannot rebind external state",
            );
        }
        if write && !symbol.mutable {
            self.error(
                D::ImmutableBinding,
                token.span,
                "cannot rebind immutable lexical name",
            );
        }
        self.capture(s, token.span, ctx);
        if let Some(ext) = self.external.get(&s) {
            self.external_nodes.insert(n, ext.clone());
        }
        let ty = if write {
            symbol.ty
        } else {
            self.refinements.get(&s).copied().unwrap_or(symbol.ty)
        };
        self.set_ty(n, ty)
    }
    fn capture(&mut self, s: SymbolId, span: TextRange, ctx: &Context) {
        let symbol = self.out.symbols[s.0 as usize].clone();
        if matches!(
            symbol.origin,
            BindingOrigin::ModuleGlobal | BindingOrigin::External
        ) {
            return;
        }
        let mut function = ctx.function;
        while let Some(f) = function {
            if !self.charge_at(span, 1) {
                return;
            }
            if symbol.origin == BindingOrigin::FunctionLocal(f) {
                break;
            }
            if let Some(&index) = self.capture_maps[f.0 as usize].get(&s) {
                self.out.functions[f.0 as usize].captures[index].used_in_default |= ctx.default;
            } else {
                if self.capture_count >= self.options.limits.max_captures {
                    self.error(D::BudgetExceeded, span, "capture budget exceeded");
                    self.exhausted = true;
                    return;
                }
                // Named fn closures are linked at block entry, before ordinary lets.
                // Read-only forward captures therefore need a shared uninitialized
                // slot too. Cell mutability remains the binding's own contract.
                let needs_cell = symbol.mutable
                    || !self.initialized[s.0 as usize]
                    || symbol.kind == SymbolKind::Function
                    || self.kind(self.out.functions[f.0 as usize].node) == HirKind::Function
                    || self.out.symbols[s.0 as usize].storage == Storage::SharedCell;
                let mode = if needs_cell {
                    CaptureMode::Cell
                } else {
                    CaptureMode::Value
                };
                if needs_cell && self.out.symbols[s.0 as usize].storage != Storage::SharedCell {
                    if !self.charge_at(span, self.capture_users[s.0 as usize].len()) {
                        return;
                    }
                    for &(user, index) in &self.capture_users[s.0 as usize] {
                        self.out.functions[user.0 as usize].captures[index].mode =
                            CaptureMode::Cell;
                    }
                }
                let index = self.out.functions[f.0 as usize].captures.len();
                self.out.functions[f.0 as usize].captures.push(Capture {
                    symbol: s,
                    mode,
                    first_use: span,
                    used_in_default: ctx.default,
                });
                self.capture_maps[f.0 as usize].insert(s, index);
                self.capture_users[s.0 as usize].push((f, index));
                self.capture_count += 1;
                self.out.symbols[s.0 as usize].storage = if mode == CaptureMode::Cell {
                    Storage::SharedCell
                } else {
                    Storage::ValueCapture
                };
            }
            function = self.out.functions[f.0 as usize].parent;
        }
    }

    fn pattern_names(&mut self, n: NodeId) -> SmallVec<[PatternName; 4]> {
        if !self.budget(n) {
            return SmallVec::new();
        }
        match self.kind(n) {
            HirKind::NamePattern | HirKind::RestPattern => self
                .name(n)
                .filter(|t| self.out.hir.name(t.name.unwrap()) != "_")
                .map(|t| {
                    smallvec![PatternName {
                        node: n,
                        name: t.name.unwrap(),
                        span: t.span,
                    }]
                })
                .unwrap_or_default(),
            HirKind::PatternField
                if !self
                    .children(n)
                    .into_iter()
                    .any(|c| is_pattern(self.kind(c))) =>
            {
                self.name(n)
                    .map(|t| {
                        smallvec![PatternName {
                            node: n,
                            name: t.name.unwrap(),
                            span: t.span,
                        }]
                    })
                    .unwrap_or_default()
            }
            HirKind::OrPattern => {
                let branches = self.children(n);
                let mut result = SmallVec::new();
                let mut expected = None;
                for branch in branches {
                    let names = self.pattern_names(branch);
                    let set: HashSet<_> = names.iter().map(|n| n.name).collect();
                    if expected.as_ref().is_some_and(|e| e != &set) {
                        self.error(
                            D::PatternBindings,
                            self.span(n),
                            "OR alternatives must bind the same names",
                        );
                    }
                    if expected.is_none() {
                        expected = Some(set);
                        result = names;
                    }
                }
                result
            }
            _ => {
                let mut result = SmallVec::new();
                let mut seen = HashSet::new();
                for c in self.children(n) {
                    if is_pattern(self.kind(c)) {
                        for name in self.pattern_names(c) {
                            if !seen.insert(name.name) {
                                self.error(
                                    D::PatternBindings,
                                    name.span,
                                    "duplicate name in pattern",
                                );
                            } else {
                                result.push(name);
                            }
                        }
                    }
                }
                result
            }
        }
    }
    fn pattern_defaults(&mut self, n: NodeId, ctx: &Context) {
        for c in self.children(n) {
            if is_pattern(self.kind(c)) {
                self.pattern_defaults(c, ctx);
            } else if is_type(self.kind(c)) {
                self.annotation(c, ctx);
            } else {
                self.visit(c, ctx);
            }
        }
        self.check(n, RuntimeChecks::PATTERN);
    }
    fn bind_pattern(
        &mut self,
        n: NodeId,
        value: TypeId,
        ctx: &Context,
        mode: PatternUse,
        mutable: bool,
    ) {
        self.pattern_defaults(n, ctx);
        let names = self.pattern_names(n);
        for name in names {
            let symbol = if matches!(mode, PatternUse::Declaration | PatternUse::Branch) {
                self.declare(Declaration {
                    name: name.name,
                    span: name.span,
                    kind: SymbolKind::Local,
                    mutable,
                    ty: ANY,
                    owner: ctx.function,
                    iteration: ctx.iteration,
                })
            } else {
                self.lookup(name.name)
            };
            if let Some(s) = symbol {
                self.out.facts[name.node.0 as usize].symbol = Some(s);
                self.initialized[s.0 as usize] = true;
                if self.kind(n) == HirKind::NamePattern {
                    self.out.symbols[s.0 as usize].ty = if mutable { ANY } else { value };
                }
            }
        }
        // Assign all OR alternatives to the same committed bindings.
        self.pattern_facts(n);
        self.pattern_type(n, value, mode);
    }
    fn pattern_facts(&mut self, n: NodeId) {
        if matches!(
            self.kind(n),
            HirKind::NamePattern | HirKind::RestPattern | HirKind::PatternField
        ) && let Some(t) = self.name(n)
            && let Some(s) = self.lookup(t.name.unwrap())
        {
            self.out.facts[n.0 as usize].symbol = Some(s);
        }
        for c in self.children(n) {
            if is_pattern(self.kind(c)) {
                self.pattern_facts(c);
            }
        }
    }
    fn pattern_type(&mut self, n: NodeId, value: TypeId, mode: PatternUse) {
        let mut projected = super::patterns::Projections::new();
        let matches = super::patterns::project(self, n, value, &mut projected);
        if !matches && mode != PatternUse::Branch && !self.exhausted {
            self.error(
                D::TypeMismatch,
                self.span(n),
                "required pattern cannot match the known value shape",
            );
        }
        projected.sort_unstable_by_key(|&(node, _)| self.out.facts[node.0 as usize].symbol);
        let mut first = 0;
        while first < projected.len() {
            let symbol = self.out.facts[projected[first].0.0 as usize].symbol;
            let mut end = first + 1;
            let mut ty = projected[first].1;
            while end < projected.len()
                && self.out.facts[projected[end].0.0 as usize].symbol == symbol
            {
                let next = projected[end].1;
                ty = if ty == ANY || next == ANY {
                    ANY
                } else if ty == next {
                    ty
                } else {
                    self.intern_type(Type::Union(vec![ty, next]))
                };
                end += 1;
            }
            for &(node, _) in &projected[first..end] {
                self.set_ty(node, ty);
                if mode == PatternUse::Assignment
                    && let Some(symbol) = symbol
                    && self.annotations.contains(&symbol)
                {
                    self.require(
                        node,
                        ty,
                        self.out.symbols[symbol.0 as usize].ty,
                        "assignment",
                    );
                }
            }
            if mode != PatternUse::Assignment
                && let Some(symbol) = symbol
                && !self.out.symbols[symbol.0 as usize].mutable
            {
                self.out.symbols[symbol.0 as usize].ty = ty;
            }
            first = end;
        }
    }

    fn location(&mut self, n: NodeId, ctx: &Context) {
        match self.kind(n) {
            HirKind::Name | HirKind::NamePattern | HirKind::RestPattern => {
                if let Some(t) = self.name(n) {
                    self.read(n, t, ctx, true);
                }
            }
            HirKind::PatternField => {
                let children = self.children(n);
                let pattern = children.iter().copied().find(|&c| is_pattern(self.kind(c)));
                if let Some(p) = pattern {
                    self.location(p, ctx);
                } else if let Some(t) = self.name(n) {
                    self.read(n, t, ctx, true);
                }
                for c in children {
                    if !is_pattern(self.kind(c)) {
                        self.visit(c, ctx);
                    }
                }
            }
            HirKind::Member | HirKind::Index => {
                self.visit(n, ctx);
                if matches!(self.member_write_types.get(&n), Some(None)) {
                    self.error(
                        D::ImmutableBinding,
                        self.span(n),
                        "host property is read-only",
                    );
                }
                self.check(n, RuntimeChecks::MUTABILITY);
                if let Form::Index { object, .. } = self.out.hir.node(n).form
                    && matches!(self.out.types[self.ty(object).0 as usize], Type::Tuple(_))
                {
                    self.error(
                        D::ImmutableBinding,
                        self.span(n),
                        "tuple elements cannot be assigned",
                    );
                }
                if ctx.init {
                    self.check(n, RuntimeChecks::WRITE_EFFECT);
                }
                if ctx.derived {
                    self.check(n, RuntimeChecks::WRITE_EFFECT);
                    let object = self.children(n).first().copied();
                    if let Some(symbol) = object.and_then(|o| self.root_symbol(o)) {
                        if !self.out.symbols[symbol.0 as usize]
                            .origin
                            .same_frame(ctx.function)
                            && !ctx.conditional
                        {
                            self.error(
                                D::InvalidEffect,
                                self.span(n),
                                "derived evaluation cannot write external objects",
                            );
                        } else if !self.fresh.contains(&symbol) {
                            self.check(n, RuntimeChecks::WRITE_EFFECT);
                        }
                    } else {
                        self.check(n, RuntimeChecks::WRITE_EFFECT);
                    }
                }
            }
            HirKind::Group
            | HirKind::GroupPattern
            | HirKind::TuplePattern
            | HirKind::ArrayPattern
            | HirKind::ObjectPattern
            | HirKind::OrPattern
            | HirKind::TypePattern => {
                for c in self.children(n) {
                    if is_type(self.kind(c)) {
                        self.annotation(c, ctx);
                    } else {
                        self.location(c, ctx);
                    }
                }
            }
            HirKind::WildcardPattern | HirKind::LiteralPattern => (),
            _ => self.error(
                D::ImmutableBinding,
                self.span(n),
                "assignment requires a location or existing mutable-name pattern",
            ),
        }
    }
}
fn is_pattern(k: HirKind) -> bool {
    matches!(
        k,
        HirKind::NamePattern
            | HirKind::WildcardPattern
            | HirKind::LiteralPattern
            | HirKind::TuplePattern
            | HirKind::GroupPattern
            | HirKind::ArrayPattern
            | HirKind::ObjectPattern
            | HirKind::PatternField
            | HirKind::RestPattern
            | HirKind::OrPattern
            | HirKind::TypePattern
    )
}
fn is_type(k: HirKind) -> bool {
    matches!(
        k,
        HirKind::NamedType
            | HirKind::TupleType
            | HirKind::UnionType
            | HirKind::FunctionType
            | HirKind::GroupType
    )
}
fn is_primitive(t: &Type) -> bool {
    matches!(
        t,
        Type::Null | Type::Bool | Type::Int | Type::Float | Type::String | Type::Tuple(_)
    )
}
fn is_number(t: TypeId) -> bool {
    t == INT || t == FLOAT
}
