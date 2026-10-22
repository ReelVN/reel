use super::*;
mod arguments;
mod results;
use crate::hir::*;
use crate::{DiagnosticCode as D, RelatedRange, SyntaxKind as K, TextRange};
use smallvec::{SmallVec, smallvec};
use std::collections::{HashMap, HashSet, VecDeque};
use std::mem::size_of;
use std::sync::Arc;

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
    names: HashMap<String, NameId>,
    types: HashMap<Type, TypeId>,
    initialized: Vec<bool>,
    annotations: HashSet<SymbolId>,
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
    // Admit the seeded name index before cloning/allocating any binding metadata.
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
    let names = hir
        .names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), NameId(i as u32)))
        .collect();
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
    let type_map = types
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, t)| (t, TypeId(i as u32)))
        .collect();
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
        if let Some(&id) = self.names.get(s) {
            return id;
        }
        let id = NameId(self.out.hir.names.len() as u32);
        self.out.hir.names.push(s.to_owned());
        self.names.insert(s.to_owned(), id);
        id
    }
    fn intern_type(&mut self, t: Type) -> TypeId {
        if let Some(&id) = self.types.get(&t) {
            return id;
        }
        let id = TypeId(self.out.types.len() as u32);
        self.types.insert(t.clone(), id);
        self.out.types.push(t);
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
    fn make_function(&mut self, n: NodeId, ctx: &Context, receiver: Receiver) -> FunctionId {
        if let Some(&id) = self.fn_nodes.get(&n) {
            return id;
        }
        let id = FunctionId(self.out.functions.len() as u32);
        let (body, kind) = match self.out.hir.node(n).form {
            Form::Function { body, kind, .. } => (body, kind),
            Form::Binding {
                initializer: Some(body),
                ..
            } => (body, FunctionKind::Plain),
            // A source fragment has an embedding-supplied function owner;
            // its Module node is the synthetic body's exact source range.
            _ if n == NodeId(0) => (n, FunctionKind::Plain),
            _ => unreachable!("function nodes have a function form"),
        };
        self.out.functions.push(Function {
            node: n,
            parent: ctx.function,
            kind,
            receiver,
            role: FunctionRole::Body,
            declared_context: DeclaredContext::default(),
            lexical_class: self.class.map(|(c, _)| c),
            parameters: Vec::new(),
            return_type: ANY,
            body,
            captures: Vec::new(),
            known_wait: false,
            dynamic_calls: false,
        });
        self.capture_maps.push(HashMap::new());
        self.fn_nodes.insert(n, id);
        self.out.facts[n.0 as usize].function = Some(id);
        id
    }
    fn prepare_function(&mut self, n: NodeId, ctx: &Context, receiver: Receiver) -> TypeId {
        if !self.budget(n) {
            return ANY;
        }
        let f = self.make_function(n, ctx, receiver);
        self.out.functions[f.0 as usize].receiver = receiver;
        let ty = self.intern_type(Type::Function(f));
        if self.signature_ready.contains(&f) {
            return self.set_ty(n, ty);
        }
        let previous = self.scope;
        self.scope = self.new_scope(Some(previous), Some(f), self.span(n));
        self.out.facts[n.0 as usize].scope = Some(self.scope);
        let inner = self.function_context(f, n, receiver);
        self.out.functions[f.0 as usize].declared_context = DeclaredContext {
            no_wait: inner.no_wait,
            init: inner.init,
            derived: inner.derived,
        };
        let Form::Function {
            parameters,
            result: return_annotation,
            ..
        } = self.out.hir.node(n).form
        else {
            unreachable!()
        };
        let params = parameters.map(|p| self.children(p)).unwrap_or_default();
        if receiver == Receiver::ExplicitSelf
            && params
                .first()
                .and_then(|&p| self.name(p))
                .map(|t| self.out.hir.name(t.name.unwrap()))
                != Some("self")
        {
            self.error(
                D::InvalidReceiver,
                self.span(n),
                "instance methods require explicit first parameter self",
            );
        }
        if receiver == Receiver::Static
            && params
                .first()
                .and_then(|&p| self.name(p))
                .map(|t| self.out.hir.name(t.name.unwrap()))
                == Some("self")
        {
            self.error(
                D::InvalidReceiver,
                self.span(n),
                "static methods do not receive self",
            );
        }
        for (index, p) in params.into_iter().enumerate() {
            if self.exhausted {
                break;
            }
            let Some(token) = self.name(p) else {
                continue;
            };
            let name = token.name.unwrap();
            let Form::Parameter {
                annotation,
                default,
                ..
            } = self.out.hir.node(p).form
            else {
                continue;
            };
            let annotation = annotation.map(|a| self.annotation(a, &inner));
            let mut pty = annotation.unwrap_or(ANY);
            if self.modifier(p, K::Ellipsis) {
                self.require(p, ARRAY, pty, "rest array binding");
            }
            if receiver == Receiver::ExplicitSelf && index == 0 {
                if let Some((class, _)) = self.class {
                    pty = self.intern_type(Type::Instance(class));
                }
                if default.is_some() || self.modifier(p, K::Ellipsis) {
                    self.error(
                        D::InvalidReceiver,
                        token.span,
                        "self cannot have a default or rest modifier",
                    );
                }
            }
            if let Some(s) = self.declare(Declaration {
                name,
                span: token.span,
                kind: if receiver == Receiver::ExplicitSelf && index == 0 {
                    SymbolKind::Receiver
                } else {
                    SymbolKind::Parameter
                },
                mutable: self.modifier(p, K::Mut),
                ty: pty,
                owner: Some(f),
                iteration: false,
            }) {
                self.initialized[s.0 as usize] = true;
                self.out.facts[p.0 as usize].symbol = Some(s);
                if annotation.is_some() {
                    self.annotations.insert(s);
                }
                let rest = self.modifier(p, K::Ellipsis);
                self.out.functions[f.0 as usize].parameters.push(Parameter {
                    symbol: s,
                    annotation,
                    default,
                    rest,
                });
            }
        }
        if receiver == Receiver::ExplicitSelf && self.class.is_some_and(|(_, derived)| derived) {
            let name = self.intern_name("super");
            if let Some(s) = self.declare(Declaration {
                name,
                span: self.span(n),
                kind: SymbolKind::Super,
                mutable: false,
                ty: ANY,
                owner: Some(f),
                iteration: false,
            }) {
                self.initialized[s.0 as usize] = true;
            }
        }
        if let Some(ret) = return_annotation {
            let result = self.annotation(ret, &inner);
            self.out.functions[f.0 as usize].return_type = result;
        }
        self.signature_ready.insert(f);
        self.scope = previous;
        self.set_ty(n, ty)
    }
    fn function_context(&self, f: FunctionId, n: NodeId, receiver: Receiver) -> Context {
        let kind = self.out.functions[f.0 as usize].kind;
        let method_name = self.name(n).map(|t| self.out.hir.name(t.name.unwrap()));
        let restricted_method = receiver == Receiver::ExplicitSelf
            && method_name.is_some_and(|s| s == "init" || (s.starts_with('_') && s != "_call"));
        // Function definitions reset dynamic execution context; constructing a
        // closure in children/finally does not constrain its eventual body.
        Context {
            function: Some(f),
            generator: kind == FunctionKind::Generator,
            no_wait: kind == FunctionKind::Generator
                || restricted_method
                || self.derived_functions.contains(&n),
            derived: self.derived_functions.contains(&n),
            ..Default::default()
        }
    }
    fn function(&mut self, n: NodeId, ctx: &Context, receiver: Receiver) -> TypeId {
        if self.exhausted {
            return ANY;
        }
        let ty = self.prepare_function(n, ctx, receiver);
        if self.exhausted {
            return ANY;
        }
        let f = self.fn_nodes[&n];
        if !self.analyzed_functions.insert(f) {
            return self.set_ty(n, ty);
        }
        let previous = self.scope;
        self.scope = self.out.facts[n.0 as usize]
            .scope
            .expect("prepared function scope");
        let inner = self.function_context(f, n, receiver);
        for index in 0..self.out.functions[f.0 as usize].parameters.len() {
            if self.exhausted {
                break;
            }
            let parameter = &self.out.functions[f.0 as usize].parameters[index];
            let (symbol, default, annotation) =
                (parameter.symbol, parameter.default, parameter.annotation);
            if let Some(default) = default {
                self.default_boundaries.push((f, symbol));
                let mut default_ctx = inner.clone();
                default_ctx.default = true;
                default_ctx.default_return_boundary = Some(default);
                default_ctx.no_wait = true;
                let actual = self.visit(default, &default_ctx);
                if let Some(expected) = annotation {
                    self.require(default, actual, expected, "parameter default");
                }
                self.default_boundaries.pop();
            }
        }
        let body = self.out.functions[f.0 as usize].body;
        if body != n {
            let result = if self.kind(body) == HirKind::Block {
                self.block(body, &inner, false)
            } else {
                self.visit(body, &inner)
            };
            let expected = self.out.functions[f.0 as usize].return_type;
            // Explicit terminal transfers do not create an implicit null return.
            if self.falls_through(body) {
                self.require(body, result, expected, "function result");
            }
        }
        self.scope = previous;
        self.set_ty(n, ty)
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
        self.set_ty(n, symbol.ty)
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
    fn annotation(&mut self, n: NodeId, ctx: &Context) -> TypeId {
        if !self.budget(n) {
            return ANY;
        }
        let children = self.children(n);
        let result = match self.kind(n) {
            HirKind::NamedType => {
                if self
                    .out
                    .hir
                    .tokens(n)
                    .filter(|t| t.kind == K::Ident)
                    .count()
                    > 1
                {
                    return self.qualified_type(n, ctx);
                }
                let text = self
                    .tokens(n)
                    .iter()
                    .map(|t| self.out.hir.text(t.span))
                    .collect::<String>();
                if let Some(builtin) = super::types::builtin_type(&text) {
                    self.hint(&builtin)
                } else {
                    if let Some(t) = self.name(n) {
                        let name = t.name.unwrap();
                        if let Some(s) = self.lookup(name) {
                            self.out.facts[n.0 as usize].symbol = Some(s);
                            self.capture(s, t.span, ctx);
                            match self.out.types[self.out.symbols[s.0 as usize].ty.0 as usize] {
                                Type::Class(class) => self.intern_type(Type::Instance(class)),
                                Type::Named(name) => self.intern_type(Type::Named(name)),
                                Type::Any
                                    if matches!(
                                        self.out.symbols[s.0 as usize].kind,
                                        SymbolKind::Host | SymbolKind::Import
                                    ) =>
                                {
                                    self.intern_type(Type::Named(name))
                                }
                                _ => {
                                    self.error(
                                        D::InvalidType,
                                        t.span,
                                        "annotation name is not a class/native type",
                                    );
                                    ANY
                                }
                            }
                        } else {
                            self.error(D::InvalidType, t.span, format!("unknown type {text}"));
                            ANY
                        }
                    } else {
                        ANY
                    }
                }
            }
            HirKind::GroupType => children.first().map_or(ANY, |&c| self.annotation(c, ctx)),
            HirKind::TupleType => {
                let ts = children
                    .into_iter()
                    .map(|c| self.annotation(c, ctx))
                    .collect();
                self.intern_type(Type::Tuple(ts))
            }
            HirKind::UnionType => {
                let mut ts = Vec::new();
                for c in children {
                    let t = self.annotation(c, ctx);
                    match &self.out.types[t.0 as usize] {
                        Type::Union(v) => ts.extend(v),
                        _ => ts.push(t),
                    }
                }
                ts.sort();
                ts.dedup();
                self.intern_type(Type::Union(ts))
            }
            HirKind::FunctionType => {
                let mut ts: Vec<_> = children
                    .into_iter()
                    .map(|c| self.annotation(c, ctx))
                    .collect();
                let r = ts.pop().unwrap_or(ANY);
                self.intern_type(Type::Callable(ts, r))
            }
            _ => ANY,
        };
        self.set_ty(n, result)
    }
    fn qualified_type(&mut self, n: NodeId, ctx: &Context) -> TypeId {
        let names = self
            .tokens(n)
            .into_iter()
            .filter_map(|t| t.name)
            .collect::<Vec<_>>();
        let Some(s) = names.first().and_then(|&name| self.lookup(name)) else {
            self.error(D::InvalidType, self.span(n), "unknown type namespace");
            return ANY;
        };
        self.out.facts[n.0 as usize].symbol = Some(s);
        self.capture(s, self.span(n), ctx);
        let mut descriptor = self.external.get(&s).cloned();
        for &name in names.iter().skip(1) {
            let Some(namespace) = descriptor.as_ref().and_then(|d| d.namespace.clone()) else {
                self.error(
                    D::InvalidType,
                    self.span(n),
                    "type path requires a namespace",
                );
                return ANY;
            };
            let mut path = namespace
                .module
                .split("::")
                .map(str::to_owned)
                .collect::<Vec<_>>();
            path.push(self.out.hir.name(name).to_owned());
            match self.resolve(&ImportRequest {
                from: self.options.module.clone(),
                path,
                kind: ImportKind::Symbol,
            }) {
                Ok(mut result) if result.version == namespace.version => {
                    descriptor = result.bindings.pop().map(ExternalDescriptor::reference)
                }
                Ok(_) => {
                    self.error(
                        D::ModuleResolution,
                        self.span(n),
                        "type namespace version changed",
                    );
                    return ANY;
                }
                Err(error) => {
                    self.error(D::ModuleResolution, self.span(n), error.to_string());
                    return ANY;
                }
            }
        }
        if let Some(descriptor) = descriptor {
            let ty = match &descriptor.ty {
                TypeHint::Named(_) => self.external_type(&descriptor),
                TypeHint::Any => ANY,
                _ => {
                    self.error(
                        D::InvalidType,
                        self.span(n),
                        "type path does not name a class/native type",
                    );
                    ANY
                }
            };
            self.external_nodes.insert(n, descriptor);
            self.set_ty(n, ty)
        } else {
            ANY
        }
    }
    // Any/union with any is uncertain, not incompatible. User-defined objects
    // may supply protocols; static rejection is limited to definite primitives.
    fn compatible(&self, actual: TypeId, expected: TypeId) -> bool {
        if actual == ANY || expected == ANY || actual == expected {
            return true;
        }
        match (
            &self.out.types[actual.0 as usize],
            &self.out.types[expected.0 as usize],
        ) {
            (_, Type::Union(ts)) => ts.iter().any(|&t| self.compatible(actual, t)),
            (Type::Union(ts), _) => ts.iter().any(|&t| self.compatible(t, expected)),
            (Type::Tuple(a), Type::Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(&a, &b)| self.compatible(a, b))
            }
            (Type::Tuple(_), Type::Named(name)) | (Type::Named(name), Type::Tuple(_))
                if self.out.hir.name(*name) == "tuple" =>
            {
                true
            }
            (Type::Function(f) | Type::BoundMethod(f), Type::Callable(p, r)) => {
                let bound = matches!(self.out.types[actual.0 as usize], Type::BoundMethod(_));
                let f = &self.out.functions[f.0 as usize];
                f.parameters.len() - usize::from(bound) == p.len()
                    && self.compatible(f.return_type, *r)
                    && f.parameters
                        .iter()
                        .skip(usize::from(bound))
                        .zip(p)
                        .all(|(a, &b)| self.compatible(self.out.symbols[a.symbol.0 as usize].ty, b))
            }
            (Type::Function(_) | Type::BoundMethod(_) | Type::Callable(_, _), Type::Closure) => {
                true
            }
            (Type::Class(_), Type::Named(name)) if self.out.hir.name(*name) == "class" => true,
            (Type::Named(actual), Type::Named(expected))
                if self.out.hir.name(*actual) == "tuple"
                    || self.out.hir.name(*expected) == "tuple" =>
            {
                actual == expected
            }
            (Type::Named(actual), Type::Named(expected)) => self
                .resolver
                .is_named_subtype(self.out.hir.name(*actual), self.out.hir.name(*expected))
                .unwrap_or(true),
            (Type::Named(_) | Type::Instance(_), Type::Named(_) | Type::Instance(_)) => true,
            _ => false,
        }
    }
    fn require(&mut self, n: NodeId, actual: TypeId, expected: TypeId, label: &str) {
        if expected == ANY {
            return;
        }
        if self.incomplete_signature(actual) || self.incomplete_signature(expected) {
            self.pending_requirements.push(PendingRequirement {
                node: n,
                actual,
                expected,
                label: label.to_owned(),
            });
            self.check(n, RuntimeChecks::TYPE);
            return;
        }
        if !self.compatible(actual, expected) {
            self.error(
                D::TypeMismatch,
                self.span(n),
                format!(
                    "{label}: {} cannot satisfy {}",
                    self.out.display_type(actual),
                    self.out.display_type(expected)
                ),
            );
        } else if actual == ANY || actual != expected {
            self.check(n, RuntimeChecks::TYPE);
        }
    }
    fn incomplete_signature(&self, ty: TypeId) -> bool {
        match &self.out.types[ty.0 as usize] {
            Type::Function(function) | Type::BoundMethod(function) => {
                !self.signature_ready.contains(function)
            }
            Type::Tuple(types) | Type::Union(types) => {
                types.iter().any(|&ty| self.incomplete_signature(ty))
            }
            Type::Callable(types, result) => {
                types.iter().any(|&ty| self.incomplete_signature(ty))
                    || self.incomplete_signature(*result)
            }
            _ => false,
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
    fn visit(&mut self, n: NodeId, ctx: &Context) -> TypeId {
        if !self.budget(n) {
            return ANY;
        }
        self.out.facts[n.0 as usize].scope = Some(self.scope);
        let children = self.children(n);
        use HirKind as H;
        let ty = match self.kind(n) {
            H::Module | H::Block => return self.block(n, ctx, true),
            H::ModifiedDeclaration => children.first().map_or(NULL, |&c| self.visit(c, ctx)),
            H::Function => return self.function(n, ctx, Receiver::None),
            H::Closure => return self.function(n, ctx, Receiver::None),
            H::Let => {
                let pattern = children.iter().copied().find(|&c| is_pattern(self.kind(c)));
                let annotation = children
                    .iter()
                    .copied()
                    .find(|&c| is_type(self.kind(c)))
                    .map(|c| self.annotation(c, ctx));
                let init = children
                    .iter()
                    .copied()
                    .find(|&c| !is_pattern(self.kind(c)) && !is_type(self.kind(c)));
                let value = init.map_or(ANY, |c| self.visit(c, ctx));
                if let (Some(init), Some(expected)) = (init, annotation) {
                    self.require(init, value, expected, "let initializer");
                }
                if let Some(p) = pattern {
                    self.bind_pattern(
                        p,
                        annotation.unwrap_or(value),
                        ctx,
                        PatternUse::Existing,
                        self.modifier(n, K::Mut),
                    );
                    if !self.modifier(n, K::Mut)
                        && self.kind(p) == HirKind::NamePattern
                        && let Some(initializer) = init
                        && let Some(descriptor) = self.external_nodes.get(&initializer).cloned()
                        && let Some(symbol) = self.out.facts[p.0 as usize].symbol
                    {
                        // Keep typing and navigation while execution retains the
                        // alias's own binding, module slot and initialized state.
                        self.external.insert(
                            symbol,
                            ExternalDescriptor {
                                kind: ExternalBindingKind::Metadata,
                                ..descriptor
                            },
                        );
                    }
                    if let Some(init) = init
                        && self.is_fresh(init)
                    {
                        for name in self.pattern_names(p) {
                            if let Some(s) = self.lookup(name.name) {
                                self.fresh.insert(s);
                            }
                        }
                    }
                    if annotation.is_some() {
                        for name in self.pattern_names(p) {
                            if let Some(s) = self.lookup(name.name) {
                                self.annotations.insert(s);
                                self.out.symbols[s.0 as usize].ty = self.ty(name.node);
                            }
                        }
                    }
                }
                NULL
            }
            H::Name => return self.name(n).map_or(ANY, |t| self.read(n, t, ctx, false)),
            H::Literal => self.literal(n),
            H::String => {
                for c in children {
                    self.visit(c, ctx);
                }
                STRING
            }
            H::Tuple => {
                let ts = children.into_iter().map(|c| self.visit(c, ctx)).collect();
                self.intern_type(Type::Tuple(ts))
            }
            H::Array => {
                for c in children {
                    self.visit(c, ctx);
                }
                ARRAY
            }
            H::Object => {
                for c in children {
                    self.visit(c, ctx);
                }
                TABLE
            }
            H::Group => {
                let t = children.first().map_or(ANY, |&c| self.visit(c, ctx));
                if let Some(&c) = children.first()
                    && let Some(ext) = self.external_nodes.get(&c)
                {
                    self.external_nodes.insert(n, ext.clone());
                }
                t
            }
            H::ExpressionStatement
            | H::Interpolation
            | H::Argument
            | H::NamedArgument
            | H::Spread => {
                let mut result = NULL;
                for c in children {
                    result = self.visit(c, ctx);
                }
                result
            }
            H::ObjectField => {
                let mut result = ANY;
                for c in children {
                    result = self.visit(c, ctx);
                }
                result
            }
            H::Prefix => self.prefix(n, &children, ctx),
            H::Binary | H::Range | H::Member | H::Index | H::Call => self.chain(n, ctx),
            H::Assignment => {
                self.assignment(n, ctx);
                NULL
            }
            H::Children => {
                let mut c = ctx.clone();
                c.no_wait = true;
                for child in children {
                    self.visit(child, &c);
                }
                ARRAY
            }
            H::If => self.condition(n, ctx, false),
            H::While => self.condition(n, ctx, true),
            H::For | H::Loop => self.loop_node(n, None, ctx),
            H::Label => {
                if let Some(&child) = children.first() {
                    self.loop_node(child, self.name(n).and_then(|t| t.name), ctx);
                }
                NULL
            }
            H::Match => self.match_node(n, &children, ctx),
            H::Try => self.try_node(n, &children, ctx),
            H::Return | H::Break | H::Continue | H::Throw => {
                self.jump(n, &children, ctx);
                NULL
            }
            H::Yield => {
                if !ctx.generator {
                    self.error(
                        D::InvalidControl,
                        self.span(n),
                        "yield requires a generator body",
                    );
                }
                if ctx.default || ctx.cleanup_boundary.is_some() {
                    self.error(
                        D::InvalidControl,
                        self.span(n),
                        "yield is forbidden in defaults/finally",
                    );
                }
                for c in children {
                    self.visit(c, ctx);
                }
                ANY
            }
            H::Class => self.class_node(n, &children, ctx),
            H::NamedType | H::TupleType | H::UnionType | H::FunctionType | H::GroupType => {
                return self.annotation(n, ctx);
            }
            H::Import => NULL,
            _ => {
                for c in children {
                    self.visit(c, ctx);
                }
                ANY
            }
        };
        self.set_ty(n, ty)
    }
    fn chain(&mut self, root: NodeId, ctx: &Context) -> TypeId {
        let mut chain = Vec::new();
        let mut node = root;
        loop {
            let receiver = match self.out.hir.node(node).form {
                Form::Binary { left, .. }
                    if matches!(self.kind(node), HirKind::Binary | HirKind::Range) =>
                {
                    left
                }
                Form::Member { object, .. } | Form::Index { object, .. } => object,
                Form::Call { callee, .. } => callee,
                _ => break,
            };
            if node != root {
                if !self.budget(node) {
                    return ANY;
                }
                self.out.facts[node.0 as usize].scope = Some(self.scope);
            }
            chain.push(node);
            node = receiver;
        }
        let mut ty = self.visit(node, ctx);
        for node in chain.into_iter().rev() {
            let children = self.children(node);
            ty = match self.kind(node) {
                HirKind::Binary | HirKind::Range => self.binary(node, &children, ty, ctx),
                HirKind::Member => self.member(node, &children, ty),
                HirKind::Index => self.index(node, &children, ty, ctx),
                HirKind::Call => self.call(node, ty, ctx),
                _ => unreachable!(),
            };
            self.set_ty(node, ty);
        }
        ty
    }
    fn index(&mut self, n: NodeId, children: &[NodeId], object: TypeId, ctx: &Context) -> TypeId {
        let mut index_ctx = ctx.clone();
        let optional = matches!(
            self.out.hir.node(n).form,
            Form::Index { optional: true, .. } | Form::Index { chain: true, .. }
        );
        index_ctx.conditional |= optional && !self.definitely_nonnull(object);
        let index = children.get(1).map_or(ANY, |&c| self.visit(c, &index_ctx));
        if optional && object == NULL {
            return self.set_ty(n, NULL);
        }
        match self.out.types[object.0 as usize].clone() {
            Type::Tuple(ts) => {
                if let Some(&c) = children.get(1) {
                    self.require(c, index, INT, "tuple index");
                }
                ts.into_iter()
                    .reduce(|a, b| if a == b { a } else { ANY })
                    .unwrap_or(ANY)
            }
            Type::Array => {
                if let Some(&c) = children.get(1) {
                    self.require(c, index, INT, "array index");
                }
                ANY
            }
            Type::Class(_) | Type::Instance(_) => {
                if let Some(&c) = children.get(1) {
                    self.require(c, index, STRING, "class member name");
                }
                self.check(n, RuntimeChecks::TYPE);
                ANY
            }
            Type::String => {
                self.error(
                    D::TypeMismatch,
                    self.span(n),
                    "string has no ordinary index; use an explicit text view",
                );
                ANY
            }
            _ => {
                self.check(n, RuntimeChecks::TYPE);
                ANY
            }
        }
    }
    fn literal(&self, n: NodeId) -> TypeId {
        for token in self.tokens(n) {
            match token.kind {
                K::Null => return NULL,
                K::True | K::False => return BOOL,
                K::Number => {
                    let text = self.out.hir.text(token.span);
                    let lower = text.to_ascii_lowercase();
                    return if !lower.starts_with("0x")
                        && !lower.starts_with("0o")
                        && !lower.starts_with("0b")
                        && text.contains(['.', 'e', 'E'])
                    {
                        FLOAT
                    } else {
                        INT
                    };
                }
                _ => (),
            }
        }
        ANY
    }
    fn prefix(&mut self, n: NodeId, children: &[NodeId], ctx: &Context) -> TypeId {
        let t = children.first().map_or(ANY, |&c| self.visit(c, ctx));
        let op = self.tokens(n).first().map(|t| t.kind);
        match op {
            Some(K::Await) => {
                if ctx.no_wait && (!ctx.conditional || ctx.cleanup_boundary.is_some()) {
                    self.error(
                        D::NoWait,
                        self.span(n),
                        "await is forbidden in this execution context",
                    );
                }
                if ctx.no_wait {
                    self.check(n, RuntimeChecks::NO_WAIT);
                }
                if let Some(f) = ctx.function {
                    self.out.functions[f.0 as usize].known_wait = true;
                }
                if is_primitive(&self.out.types[t.0 as usize]) {
                    self.error(
                        D::TypeMismatch,
                        self.span(n),
                        "await requires Awaitable, not a primitive value",
                    );
                }
                self.check(n, RuntimeChecks::TYPE);
                if let Some(name) = self.nullable_named_type(t) {
                    if !self.await_types.contains_key(&name) {
                        let hint = self.resolver.resolve_type_await(self.out.hir.name(name));
                        let result = hint.as_ref().map(|hint| self.hint(hint));
                        self.await_types.insert(name, result);
                    }
                    if let Some(result) = self.await_types[&name] {
                        return result;
                    }
                }
                ANY
            }
            Some(K::Bang) => {
                if let Some(&c) = children.first() {
                    self.require(c, t, BOOL, "logical negation");
                }
                BOOL
            }
            Some(K::Tilde) => {
                if let Some(&c) = children.first() {
                    self.require(c, t, INT, "bitwise negation");
                }
                INT
            }
            Some(K::Plus | K::Minus) => {
                if t == INT || t == FLOAT {
                    t
                } else if is_primitive(&self.out.types[t.0 as usize]) {
                    self.error(
                        D::TypeMismatch,
                        self.span(n),
                        "numeric unary operator requires a number",
                    );
                    ANY
                } else {
                    self.check(n, RuntimeChecks::TYPE);
                    ANY
                }
            }
            _ => ANY,
        }
    }
    fn binary(&mut self, n: NodeId, children: &[NodeId], a: TypeId, ctx: &Context) -> TypeId {
        let op = self.tokens(n).first().map(|t| t.kind);
        let mut right_ctx = ctx.clone();
        right_ctx.conditional |= match op {
            Some(K::AndAnd) => children.first().and_then(|&n| self.constant_bool(n)) != Some(true),
            Some(K::OrOr) => children.first().and_then(|&n| self.constant_bool(n)) != Some(false),
            Some(K::QuestionQuestion) => a != NULL,
            _ => false,
        };
        let b = children.get(1).map_or(ANY, |&c| self.visit(c, &right_ctx));
        match op {
            Some(K::Is) => BOOL,
            Some(K::EqEq | K::NotEq) => BOOL,
            Some(K::AndAnd | K::OrOr) => {
                for (&c, t) in children.iter().zip([a, b]) {
                    self.require(c, t, BOOL, "logical operand");
                }
                BOOL
            }
            Some(K::QuestionQuestion) => {
                if a == NULL {
                    b
                } else if self.definitely_nonnull(a) || a == b {
                    a
                } else {
                    ANY
                }
            }
            Some(K::Range | K::RangeInclusive) => {
                for (&c, t) in children.iter().zip([a, b]) {
                    self.require(c, t, INT, "range bound");
                }
                let name = self.intern_name("Range");
                self.intern_type(Type::Named(name))
            }
            Some(K::Shl | K::Shr | K::Amp | K::Pipe | K::Caret) => {
                for (&c, t) in children.iter().zip([a, b]) {
                    self.require(c, t, INT, "bitwise operand");
                }
                INT
            }
            Some(K::Lt | K::Le | K::Gt | K::Ge) => {
                if is_primitive(&self.out.types[a.0 as usize])
                    && is_primitive(&self.out.types[b.0 as usize])
                    && !((is_number(a) && is_number(b)) || (a == STRING && b == STRING))
                {
                    self.error(
                        D::TypeMismatch,
                        self.span(n),
                        "incompatible comparison operands",
                    );
                }
                BOOL
            }
            Some(K::Plus | K::Minus | K::Star | K::Slash | K::Percent) => {
                if is_number(a) && is_number(b) {
                    if a == FLOAT || b == FLOAT { FLOAT } else { INT }
                } else if op == Some(K::Plus) && a == STRING && b == STRING {
                    STRING
                } else if is_primitive(&self.out.types[a.0 as usize])
                    && is_primitive(&self.out.types[b.0 as usize])
                {
                    self.error(
                        D::TypeMismatch,
                        self.span(n),
                        "incompatible arithmetic operands",
                    );
                    ANY
                } else {
                    self.check(n, RuntimeChecks::TYPE);
                    ANY
                }
            }
            _ => ANY,
        }
    }
    fn assignment(&mut self, n: NodeId, ctx: &Context) -> TypeId {
        let children = self.children(n);
        let Some((&target, rest)) = children.split_first() else {
            return ANY;
        };
        self.location(target, ctx);
        let rhs = rest.first().copied();
        let value = rhs.map_or(ANY, |c| {
            if self.kind(c) == HirKind::Assignment {
                self.assignment(c, ctx)
            } else {
                self.visit(c, ctx)
            }
        });
        if is_pattern(self.kind(target)) {
            self.pattern_names(target);
            self.check(target, RuntimeChecks::PATTERN);
        }
        self.pattern_type(target, value, PatternUse::Assignment);
        if let Some(Some(expected)) = self.member_write_types.get(&target).copied()
            && let Some(rhs) = rhs
        {
            self.require(rhs, value, expected, "host property assignment");
        }
        value
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
    fn member(&mut self, n: NodeId, children: &[NodeId], object: TypeId) -> TypeId {
        let key = self
            .name(n)
            .map(|t| self.out.hir.name(t.name.unwrap()).to_owned());
        if let (Some(&base), Some(key)) = (children.first(), key) {
            if let Some(group) = super::builtin_members(&self.out.hir, &self.out.types, object)
                // A table's own named entry wins over a builtin method. Its
                // dynamic shape cannot promise this fallback's call contract.
                && group.receiver != TypeHint::Table
                && let Some(member) = group.members.iter().find(|member| member.symbol.name == key)
            {
                let symbol = member.symbol.clone();
                let ty = self.hint(&symbol.ty);
                self.member_write_types.insert(n, None);
                self.external_nodes
                    .insert(n, ExternalDescriptor::metadata(symbol));
                if matches!(self.out.types[object.0 as usize], Type::Union(_)) {
                    self.check(n, RuntimeChecks::TYPE);
                }
                return ty;
            }
            if let Some(class) = self
                .external_nodes
                .get(&base)
                .and_then(|symbol| symbol.class.clone())
            {
                if let Some(member) = class
                    .static_members
                    .iter()
                    .find(|member| member.symbol.name == key)
                    .cloned()
                {
                    let ty = self.hint(&member.symbol.ty);
                    let write = member.write_type.as_ref().map(|ty| self.hint(ty));
                    self.member_write_types.insert(n, write);
                    self.external_nodes
                        .insert(n, ExternalDescriptor::metadata(member.symbol));
                    return ty;
                }
                self.check(n, RuntimeChecks::TYPE);
                return ANY;
            }
            let named = self.nullable_named_type(object);
            if let Some(name) = named
                && let Some(member) = self
                    .resolver
                    .resolve_type_member(self.out.hir.name(name), &key)
            {
                let ty = self.hint(&member.symbol.ty);
                let write = member.write_type.as_ref().map(|ty| self.hint(ty));
                self.member_write_types.insert(n, write);
                self.external_nodes
                    .insert(n, ExternalDescriptor::metadata(member.symbol));
                if matches!(self.out.types[object.0 as usize], Type::Union(_)) {
                    self.check(n, RuntimeChecks::TYPE);
                    if matches!(
                        self.out.hir.node(n).form,
                        Form::Member { optional: true, .. } | Form::Member { chain: true, .. }
                    ) {
                        return self.nullable_type(ty);
                    }
                }
                return ty;
            }
            if let Some(namespace) = self
                .external_nodes
                .get(&base)
                .and_then(|e| e.namespace.clone())
            {
                let mut path = namespace
                    .module
                    .split("::")
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                path.push(key.clone());
                let request = ImportRequest {
                    from: self.options.module.clone(),
                    path,
                    kind: ImportKind::Symbol,
                };
                match self.resolve(&request) {
                    Ok(mut result) if result.version == namespace.version => {
                        if let Some(symbol) = result.bindings.pop() {
                            if symbol.type_only {
                                self.error(
                                    D::InvalidType,
                                    self.span(n),
                                    "a type declaration cannot be read as a value",
                                );
                                self.external_nodes
                                    .insert(n, ExternalDescriptor::reference(symbol));
                                return ANY;
                            }
                            let ty = self.external_type(&symbol);
                            self.external_nodes
                                .insert(n, ExternalDescriptor::reference(symbol));
                            return ty;
                        }
                    }
                    Ok(_) => self.error(
                        D::ModuleResolution,
                        self.span(n),
                        "namespace version changed during analysis",
                    ),
                    Err(error) => self.error(D::ModuleResolution, self.span(n), error.to_string()),
                }
            }
            if let Type::Class(class) | Type::Instance(class) =
                self.out.types[object.0 as usize].clone()
            {
                let instance = matches!(self.out.types[object.0 as usize], Type::Instance(_));
                let name = self.intern_name(&key);
                if let Some(d) = self.find_class_member(class, name, !instance) {
                    if self.kind(d) == HirKind::Function
                        && let Some(f) = self.fn_nodes.get(&d).copied()
                    {
                        self.check(n, RuntimeChecks::RECEIVER);
                        self.out.facts[n.0 as usize].function = Some(f);
                        return self.intern_type(if instance {
                            Type::BoundMethod(f)
                        } else {
                            Type::Function(f)
                        });
                    }
                    return self.ty(d);
                }
            }
        }
        self.check(n, RuntimeChecks::TYPE);
        ANY
    }
    fn find_class_member(
        &self,
        mut class: NodeId,
        name: NameId,
        static_member: bool,
    ) -> Option<NodeId> {
        for _ in 0..self.options.limits.max_depth.max(1) {
            if let Some(&node) = self
                .class_members
                .get(&class)
                .and_then(|m| m.get(&(name, static_member)))
            {
                return Some(node);
            }
            class = *self.class_bases.get(&class)?;
        }
        None
    }
    fn nullable_named_type(&self, ty: TypeId) -> Option<NameId> {
        match &self.out.types[ty.0 as usize] {
            Type::Named(name) => Some(*name),
            Type::Union(types) => {
                let mut name = None;
                for ty in types {
                    match self.out.types[ty.0 as usize] {
                        Type::Null => {}
                        Type::Named(candidate) if name.is_none_or(|name| name == candidate) => {
                            name = Some(candidate)
                        }
                        _ => return None,
                    }
                }
                name
            }
            _ => None,
        }
    }
    fn nullable_type(&mut self, ty: TypeId) -> TypeId {
        match &self.out.types[ty.0 as usize] {
            Type::Null => ty,
            Type::Union(types) if types.contains(&NULL) => ty,
            _ => self.intern_type(Type::Union(vec![ty, NULL])),
        }
    }
    fn call(&mut self, n: NodeId, ty: TypeId, ctx: &Context) -> TypeId {
        let Form::Call {
            callee: target,
            arguments,
            children: trailing,
            optional,
            chain,
        } = self.out.hir.node(n).form
        else {
            return ANY;
        };
        let mut descriptor = self.external_nodes.get(&target).cloned();
        let mut named_callee = None;
        if descriptor
            .as_ref()
            .is_none_or(|descriptor| descriptor.signature.is_none())
            && let Some(name) = self.nullable_named_type(ty)
        {
            if !self.callable_types.contains_key(&name) {
                let callable = self
                    .resolver
                    .resolve_type_call(self.out.hir.name(name))
                    .map(ExternalDescriptor::metadata);
                self.callable_types.insert(name, callable);
            }
            if let Some(callable) = &self.callable_types[&name] {
                descriptor = Some(callable.clone());
                named_callee = Some(name);
            }
        }
        if let Some(Intrinsic::Derive) = descriptor.as_ref().and_then(|d| d.intrinsic)
            && let Some(args) = arguments
        {
            for arg in self.children(args) {
                let mut value = self.children(arg).last().copied().unwrap_or(arg);
                while let Form::Single { child } = self.out.hir.node(value).form {
                    value = child;
                }
                if self.kind(value) == HirKind::Closure {
                    self.derived_functions.insert(value);
                }
            }
        }
        let mut actual_ctx = ctx.clone();
        if optional || chain {
            actual_ctx.conditional = true;
        }
        for c in arguments.into_iter().chain(trailing) {
            self.visit(c, &actual_ctx);
        }
        if let Some(arguments) = arguments {
            for arg in self.children(arguments) {
                if self.kind(arg) == HirKind::Spread {
                    self.check_argument_spread(arg);
                }
            }
        }
        if let Some(intrinsic) = descriptor.as_ref().and_then(|d| d.intrinsic) {
            self.out.facts[n.0 as usize].intrinsic = Some(intrinsic);
            if intrinsic == Intrinsic::Bind {
                let value = arguments
                    .and_then(|args| self.children(args).first().copied())
                    .and_then(|a| self.children(a).first().copied());
                if let Some(value) = value {
                    if let Form::Member { .. } | Form::Index { .. } = self.out.hir.node(value).form
                    {
                        // An ordinary table alias can be adopted by a model later.
                        // Observable identity is therefore checked at execution time.
                        self.check(value, RuntimeChecks::PROPERTY);
                    } else {
                        self.error(
                            D::TypeMismatch,
                            self.span(value),
                            "bind requires an observable property location",
                        );
                    }
                } else {
                    self.error(
                        D::InvalidCall,
                        self.span(n),
                        "bind requires a property location",
                    );
                }
            }
        }
        if ctx.derived {
            self.check(n, RuntimeChecks::DERIVED_SAFE);
        }
        if ctx.init {
            self.check(n, RuntimeChecks::INIT_SAFE);
        }
        if !ctx.conditional
            && let Some(signature) = descriptor.as_ref().and_then(|d| d.signature.as_ref())
            && ((ctx.derived && signature.derived_safe == Some(false))
                || (ctx.init && signature.init_safe == Some(false)))
        {
            self.error(
                D::InvalidEffect,
                self.span(n),
                "host function is not safe in this execution context",
            );
        }
        if ty == NULL && (optional || chain) {
            return NULL;
        }
        let bound = matches!(
            self.out.types[ty.0 as usize],
            Type::BoundMethod(_) | Type::Class(_)
        );
        let constructor = if let Type::Class(class) = self.out.types[ty.0 as usize] {
            let init = self.intern_name("init");
            self.find_class_member(class, init, false)
                .and_then(|n| self.fn_nodes.get(&n).copied())
        } else {
            None
        };
        let function = match self.out.types[ty.0 as usize] {
            Type::Function(f) | Type::BoundMethod(f) => Some(f),
            _ => constructor,
        };
        let external = descriptor.clone();
        let effect = external
            .as_ref()
            .and_then(|e| e.signature.as_ref())
            .map_or(CallEffect::Unknown, |s| s.effect);
        let call = CallSite {
            node: n,
            caller: ctx.function,
            callee: function,
            bound,
            effect,
            no_wait: ctx.no_wait,
            named_callee,
        };
        self.calls.push(call.clone());
        self.check(n, RuntimeChecks::CALL);
        if ctx.no_wait {
            self.check(n, RuntimeChecks::NO_WAIT);
        }
        if matches!(
            descriptor.as_ref().and_then(|symbol| symbol.intrinsic),
            Some(Intrinsic::Bind | Intrinsic::Derive)
        ) {
            let name = self.intern_name("Binding");
            return self.intern_type(Type::Named(name));
        }
        if let Some(external) = external
            && let Some(signature) = &external.signature
        {
            if (!signature.result_cases.is_empty() || signature.result_parameter.is_some())
                && let Some(result) =
                    self.specialized_call_result(&call, target, arguments, trailing, signature)
            {
                return if (optional || chain) && !self.definitely_nonnull(ty) {
                    self.nullable_type(result)
                } else {
                    result
                };
            }
            let result = self.hint(&signature.result);
            return if (optional || chain) && !self.definitely_nonnull(ty) {
                self.nullable_type(result)
            } else {
                result
            };
        }
        if let Type::Class(class) = self.out.types[ty.0 as usize] {
            self.check(n, RuntimeChecks::NO_WAIT);
            return self.intern_type(Type::Instance(class));
        }
        if let Some(f) = function {
            return match self.out.functions[f.0 as usize].kind {
                FunctionKind::Async => FUTURE,
                FunctionKind::Generator => GENERATOR,
                FunctionKind::Plain => self.out.functions[f.0 as usize].return_type,
            };
        }
        if let Type::Class(class) = self.out.types[ty.0 as usize] {
            return self.intern_type(Type::Instance(class));
        }
        if let Type::Callable(_, r) = self.out.types[ty.0 as usize] {
            return r;
        }
        if is_primitive(&self.out.types[ty.0 as usize]) {
            self.error(
                D::InvalidCall,
                self.span(n),
                "primitive value is not callable",
            );
        }
        ANY
    }
    fn condition(&mut self, n: NodeId, ctx: &Context, is_loop: bool) -> TypeId {
        let Form::Condition {
            pattern,
            condition,
            body,
            otherwise,
        } = self.out.hir.node(n).form
        else {
            return ANY;
        };
        let mut inner = ctx.clone();
        if is_loop && inner.loops.last().is_none_or(|(node, _, _)| *node != n) {
            // The condition is inside its loop's control boundary as well.
            // Labelled while already installed this exact target in loop_node.
            inner.loops.push((n, None, ctx.finally.len()));
        }
        if is_loop {
            // Both the condition and body are reentered through the loop head.
            inner.iteration = true;
        }
        let value = self.visit(condition, if is_loop { &inner } else { ctx });
        if pattern.is_none() {
            self.require(condition, value, BOOL, "condition");
        }
        let previous = self.scope;
        self.scope = self.new_scope(Some(previous), ctx.function, self.span(n));
        if let Some(p) = pattern {
            self.bind_pattern(p, value, &inner, PatternUse::Branch, false);
        }
        let constant = self.constant_bool(condition);
        inner.conditional |= constant != Some(true);
        let yes = self.visit(body, &inner);
        self.scope = previous;
        let mut otherwise_ctx = ctx.clone();
        otherwise_ctx.conditional |= constant != Some(false);
        let no = otherwise.map_or(NULL, |c| self.visit(c, &otherwise_ctx));
        if is_loop {
            NULL
        } else if yes == no {
            yes
        } else {
            ANY
        }
    }
    fn loop_node(&mut self, n: NodeId, label: Option<NameId>, ctx: &Context) -> TypeId {
        if self.kind(n) == HirKind::While {
            let mut c = ctx.clone();
            c.loops.push((n, label, ctx.finally.len()));
            self.condition(n, &c, true);
            return NULL;
        }
        let mut inner = ctx.clone();
        if label.is_some_and(|label| ctx.loops.iter().any(|(_, old, _)| *old == Some(label))) {
            self.error(
                D::InvalidControl,
                self.span(n),
                "duplicate active loop label",
            );
        }
        inner.loops.push((n, label, ctx.finally.len()));
        inner.iteration = true;
        inner.conditional |= self.kind(n) == HirKind::For;
        let (pattern, source, body, asynchronous) = match self.out.hir.node(n).form {
            Form::For {
                pattern,
                source,
                body,
                asynchronous,
            } => (Some(pattern), Some(source), body, asynchronous),
            Form::Single { child } => (None, None, child, false),
            _ => return ANY,
        };
        let value = source.map_or(ANY, |c| self.visit(c, ctx));
        if asynchronous {
            if ctx.no_wait && (!ctx.conditional || ctx.cleanup_boundary.is_some()) {
                self.error(
                    D::NoWait,
                    self.span(n),
                    "for await is forbidden in this execution context",
                );
            }
            if ctx.no_wait {
                self.check(n, RuntimeChecks::NO_WAIT);
            }
            if let Some(f) = ctx.function {
                self.out.functions[f.0 as usize].known_wait = true;
            }
        }
        let previous = self.scope;
        self.scope = self.new_scope(Some(previous), ctx.function, self.span(n));
        if let Some(p) = pattern {
            let item = match &self.out.types[value.0 as usize] {
                Type::Tuple(ts) => ts
                    .iter()
                    .copied()
                    .reduce(|a, b| if a == b { a } else { ANY })
                    .unwrap_or(ANY),
                _ => ANY,
            };
            self.bind_pattern(p, item, &inner, PatternUse::Declaration, false);
        }
        self.visit(body, &inner);
        self.scope = previous;
        NULL
    }
    fn match_node(&mut self, n: NodeId, children: &[NodeId], ctx: &Context) -> TypeId {
        let value = children.first().map_or(ANY, |&c| self.visit(c, ctx));
        let mut result = None;
        for &arm in children.iter().skip(1) {
            let previous = self.scope;
            self.scope = self.new_scope(Some(previous), ctx.function, self.span(arm));
            self.out.facts[arm.0 as usize].scope = Some(self.scope);
            let parts = self.children(arm);
            if let Some(&p) = parts.first() {
                self.bind_pattern(p, value, ctx, PatternUse::Branch, false);
            }
            if self.token(arm, K::If).is_some()
                && let Some(&guard) = parts.get(1)
            {
                let mut c = ctx.clone();
                c.no_wait = true;
                c.conditional = true;
                let t = self.visit(guard, &c);
                self.require(guard, t, BOOL, "match guard");
            }
            let mut branch = ctx.clone();
            branch.conditional = true;
            let t = parts.last().map_or(NULL, |&body| self.visit(body, &branch));
            result = Some(match result {
                None => t,
                Some(old) if old == t => t,
                _ => ANY,
            });
            self.scope = previous;
        }
        self.check(n, RuntimeChecks::PATTERN);
        result.unwrap_or(ANY)
    }
    fn try_node(&mut self, _n: NodeId, children: &[NodeId], ctx: &Context) -> TypeId {
        let finally = children
            .iter()
            .copied()
            .find(|&c| self.kind(c) == HirKind::Finally);
        let mut protected = ctx.clone();
        if let Some(f) = finally {
            protected.finally.push(f);
        }
        if let Some(&body) = children.first() {
            self.visit(body, &protected);
        }
        for &handler in children.iter().skip(1) {
            let previous = self.scope;
            self.scope = self.new_scope(Some(previous), ctx.function, self.span(handler));
            let mut c = protected.clone();
            if self.kind(handler) == HirKind::Catch {
                c.catch = true;
                if let Some(t) = self.name(handler)
                    && let Some(s) = self.declare(Declaration {
                        name: t.name.unwrap(),
                        span: t.span,
                        kind: SymbolKind::Catch,
                        mutable: false,
                        ty: ANY,
                        owner: ctx.function,
                        iteration: ctx.iteration,
                    })
                {
                    self.initialized[s.0 as usize] = true;
                    self.out.facts[handler.0 as usize].symbol = Some(s);
                }
            } else {
                c = ctx.clone();
                c.no_wait = true;
                c.cleanup_boundary = Some(c.loops.len());
            }
            for body in self.children(handler) {
                self.visit(body, &c);
            }
            self.scope = previous;
        }
        NULL
    }
    fn jump(&mut self, n: NodeId, children: &[NodeId], ctx: &Context) {
        let mut target = None;
        let mut cleanup = ctx.finally.iter().rev().copied().collect::<Vec<_>>();
        match self.kind(n) {
            HirKind::Return => {
                if let Some(default) = ctx.default_return_boundary {
                    let previous_diagnostics = self.diagnostics.len();
                    let boundary = self.span(default);
                    self.error(
                        D::InvalidDefault,
                        self.span(n),
                        "explicit return cannot leave a parameter default expression",
                    );
                    if self.diagnostics.len() > previous_diagnostics {
                        self.diagnostics
                            .last_mut()
                            .unwrap()
                            .related
                            .push(RelatedRange {
                                range: boundary,
                                message: "parameter default expression boundary".into(),
                            });
                    }
                    for &child in children {
                        self.visit(child, ctx);
                    }
                    // An invalid source return does not target the declaring
                    // function or get checked against its return contract.
                    self.out.facts[n.0 as usize].control = Some(ControlTransfer {
                        target: None,
                        finally: cleanup,
                    });
                    return;
                }
                if ctx.function.is_none() {
                    self.error(
                        D::InvalidControl,
                        self.span(n),
                        "return requires a function",
                    );
                }
                if ctx.cleanup_boundary.is_some() {
                    self.error(
                        D::InvalidControl,
                        self.span(n),
                        "return cannot leave finally",
                    );
                }
                if let Some(f) = ctx.function {
                    target = Some(self.out.functions[f.0 as usize].node);
                    let t = children.first().map_or(NULL, |&c| self.visit(c, ctx));
                    let expected = self.out.functions[f.0 as usize].return_type;
                    self.require(
                        children.first().copied().unwrap_or(n),
                        t,
                        expected,
                        "return",
                    );
                } else {
                    for &c in children {
                        self.visit(c, ctx);
                    }
                }
            }
            HirKind::Break | HirKind::Continue => {
                let label = self.name(n).and_then(|t| t.name);
                let found = ctx
                    .loops
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, (_, l, _))| label.is_none() || *l == label);
                if let Some((index, (node, _, depth))) = found {
                    target = Some(*node);
                    cleanup = ctx.finally[*depth..].iter().rev().copied().collect();
                    if ctx
                        .cleanup_boundary
                        .is_some_and(|boundary| index < boundary)
                    {
                        self.error(D::InvalidControl, self.span(n), "jump cannot leave finally");
                    }
                } else {
                    self.error(
                        D::InvalidControl,
                        self.span(n),
                        "jump requires a matching enclosing loop",
                    );
                }
            }
            HirKind::Throw => {
                if children.is_empty() && !ctx.catch {
                    self.error(
                        D::InvalidControl,
                        self.span(n),
                        "throw without a value requires catch",
                    );
                }
                for &c in children {
                    self.visit(c, ctx);
                }
            }
            _ => (),
        }
        self.out.facts[n.0 as usize].control = Some(ControlTransfer {
            target,
            finally: cleanup,
        });
    }
    fn root_symbol(&self, n: NodeId) -> Option<SymbolId> {
        if let Some(s) = self.out.facts[n.0 as usize].symbol {
            return Some(s);
        }
        match self.out.hir.node(n).form {
            Form::Member { object, .. }
            | Form::Index { object, .. }
            | Form::Single { child: object } => self.root_symbol(object),
            _ => None,
        }
    }
    fn is_fresh(&self, n: NodeId) -> bool {
        if matches!(self.kind(n), HirKind::Object | HirKind::Array) {
            return true;
        }
        if let Some(s) = self.out.facts[n.0 as usize].symbol {
            return self.fresh.contains(&s);
        }
        if let Form::Single { child } = self.out.hir.node(n).form {
            return self.is_fresh(child);
        }
        false
    }
    fn constant_bool(&self, n: NodeId) -> Option<bool> {
        match self.out.hir.node(n).form {
            Form::Literal { token } => match token.kind {
                K::True => Some(true),
                K::False => Some(false),
                _ => None,
            },
            Form::Single { child } => self.constant_bool(child),
            Form::Unary { operator, value } if operator.kind == K::Bang => {
                self.constant_bool(value).map(|b| !b)
            }
            _ => None,
        }
    }
    fn definitely_nonnull(&self, ty: TypeId) -> bool {
        match &self.out.types[ty.0 as usize] {
            Type::Any | Type::Null => false,
            Type::Union(types) => types.iter().all(|&ty| self.definitely_nonnull(ty)),
            _ => true,
        }
    }
    fn falls_through(&self, n: NodeId) -> bool {
        match self.out.hir.node(n).form {
            Form::Transfer { .. } => !matches!(
                self.kind(n),
                HirKind::Return | HirKind::Throw | HirKind::Break | HirKind::Continue
            ),
            Form::Statement { expression, .. } | Form::Single { child: expression }
                if self.kind(n) != HirKind::Loop =>
            {
                self.falls_through(expression)
            }
            Form::Block { .. } | Form::Sequence
                if matches!(self.kind(n), HirKind::Block | HirKind::Module) =>
            {
                self.out.hir.children(n).all(|c| self.falls_through(c))
            }
            Form::Condition {
                condition,
                body,
                otherwise,
                ..
            } if self.kind(n) == HirKind::If => match self.constant_bool(condition) {
                Some(true) => self.falls_through(body),
                Some(false) => otherwise.is_none_or(|body| self.falls_through(body)),
                None => {
                    self.falls_through(body)
                        || otherwise.is_none_or(|body| self.falls_through(body))
                }
            },
            Form::Condition {
                condition, body, ..
            } if self.kind(n) == HirKind::While => {
                self.constant_bool(condition) != Some(true) || self.can_break_to(body, n)
            }
            Form::Single { child } if self.kind(n) == HirKind::Loop => self.can_break_to(child, n),
            Form::Try {
                body,
                catch,
                finally,
            } => {
                finally.is_none_or(|cleanup| self.falls_through(cleanup))
                    && (self.falls_through(body)
                        || catch.is_some_and(|catch| self.falls_through(catch)))
            }
            Form::Catch { body, .. } | Form::Label { body, .. } => self.falls_through(body),
            Form::Match { .. } => self
                .out
                .hir
                .children(n)
                .skip(1)
                .any(|arm| self.falls_through(arm)),
            Form::MatchArm { body, .. } => self.falls_through(body),
            _ => true,
        }
    }
    fn can_break_to(&self, n: NodeId, target: NodeId) -> bool {
        if self.kind(n) == HirKind::Break {
            return self.out.facts[n.0 as usize]
                .control
                .as_ref()
                .is_some_and(|flow| flow.target == Some(target));
        }
        if matches!(
            self.kind(n),
            HirKind::Function | HirKind::Closure | HirKind::Class
        ) {
            return false;
        }
        if let Form::Condition {
            condition,
            body,
            otherwise,
            ..
        } = self.out.hir.node(n).form
            && self.kind(n) == HirKind::If
        {
            return match self.constant_bool(condition) {
                Some(true) => self.can_break_to(body, target),
                Some(false) => otherwise.is_some_and(|body| self.can_break_to(body, target)),
                None => {
                    self.can_break_to(body, target)
                        || otherwise.is_some_and(|body| self.can_break_to(body, target))
                }
            };
        }
        if let Form::Try {
            finally: Some(finally),
            ..
        } = self.out.hir.node(n).form
            && !self.falls_through(finally)
        {
            return self.can_break_to(finally, target);
        }
        for child in self.out.hir.children(n) {
            if self.can_break_to(child, target) {
                return true;
            }
            if matches!(self.kind(n), HirKind::Block | HirKind::Module)
                && !self.falls_through(child)
            {
                break;
            }
        }
        false
    }
    fn prepare_class(&mut self, n: NodeId, ctx: &Context) {
        if !self.prepared_classes.insert(n) {
            return;
        }
        let children = self.children(n);
        let base = children
            .iter()
            .copied()
            .find(|&c| !matches!(self.kind(self.unwrap(c)), HirKind::Let | HirKind::Function));
        if let Some(base) = base
            && let Form::Name { name } = self.out.hir.node(base).form
            && let Some(symbol) = name.name.and_then(|name| self.lookup(name))
            && let Type::Class(parent) =
                self.out.types[self.out.symbols[symbol.0 as usize].ty.0 as usize]
        {
            self.class_bases.insert(n, parent);
        }
        let old_class = self.class;
        self.class = Some((n, base.is_some()));
        let mut seen = HashSet::new();
        for &member in &children {
            if !self.budget(member) {
                break;
            }
            let d = self.unwrap(member);
            if !matches!(self.kind(d), HirKind::Function | HirKind::Let) {
                continue;
            }
            let name = if self.kind(d) == HirKind::Let {
                self.children(d)
                    .into_iter()
                    .find(|&c| is_pattern(self.kind(c)))
                    .and_then(|c| self.name(c))
            } else {
                self.name(d)
            };
            if let Some(name) = name {
                let static_member = self.modifier(member, K::Static);
                self.class_members
                    .entry(n)
                    .or_default()
                    .insert((name.name.unwrap(), static_member), d);
            }
            if let Some(name) = name
                && !seen.insert((name.name, self.modifier(member, K::Static)))
            {
                self.error(D::DuplicateBinding, name.span, "duplicate class member");
            }
            if self.kind(d) == HirKind::Function {
                self.make_function(
                    d,
                    ctx,
                    if self.modifier(member, K::Static) {
                        Receiver::Static
                    } else {
                        Receiver::ExplicitSelf
                    },
                );
            } else if let Form::Binding {
                annotation: Some(annotation),
                ..
            } = self.out.hir.node(d).form
            {
                // Fields are visible to methods regardless of declaration order.
                let ty = self.annotation(annotation, ctx);
                self.set_ty(d, ty);
            }
        }
        for &member in &children {
            if self.exhausted {
                break;
            }
            let d = self.unwrap(member);
            if self.kind(d) == HirKind::Function {
                self.prepare_function(
                    d,
                    ctx,
                    if self.modifier(member, K::Static) {
                        Receiver::Static
                    } else {
                        Receiver::ExplicitSelf
                    },
                );
            }
        }
        self.class = old_class;
    }
    fn class_node(&mut self, n: NodeId, children: &[NodeId], ctx: &Context) -> TypeId {
        self.prepare_class(n, ctx);
        if let Form::Class {
            base: Some(base), ..
        } = self.out.hir.node(n).form
        {
            // Declaration execution, rather than signature preparation, owns
            // initialization and base-value diagnostics.
            let value = self.visit(base, ctx);
            if let Type::Class(parent) = self.out.types[value.0 as usize] {
                self.class_bases.insert(n, parent);
            }
            if matches!(self.out.types[value.0 as usize],Type::Class(class) if class==n) {
                self.error(
                    D::TypeMismatch,
                    self.span(base),
                    "class cannot inherit itself",
                );
            }
            if is_primitive(&self.out.types[value.0 as usize]) {
                self.error(D::TypeMismatch, self.span(base), "base must be a class");
            }
        }
        let old_class = self.class;
        self.class = Some((n, self.class_bases.contains_key(&n)));
        for &member in children {
            if !self.budget(member) {
                break;
            }
            let d = self.unwrap(member);
            match self.kind(d) {
                HirKind::Function => {
                    self.function(
                        d,
                        ctx,
                        if self.modifier(member, K::Static) {
                            Receiver::Static
                        } else {
                            Receiver::ExplicitSelf
                        },
                    );
                }
                HirKind::Let => {
                    let Form::Binding {
                        initializer,
                        annotation,
                        mutable,
                        ..
                    } = self.out.hir.node(d).form
                    else {
                        continue;
                    };
                    if let Some(init) = initializer {
                        let previous = self.scope;
                        let f = self.make_function(d, ctx, Receiver::None);
                        self.out.functions[f.0 as usize].role = FunctionRole::FieldInitializer;
                        self.out.functions[f.0 as usize].declared_context.no_wait = true;
                        self.out.functions[f.0 as usize].body = init;
                        self.scope = self.new_scope(Some(previous), Some(f), self.span(d));
                        let c = Context {
                            function: Some(f),
                            no_wait: true,
                            // This synthetic initializer is not a source fn.
                            default_return_boundary: ctx.default_return_boundary,
                            ..Default::default()
                        };
                        if !self.modifier(member, K::Static) {
                            let self_name = self.intern_name("self");
                            let instance = self.intern_type(Type::Instance(n));
                            if let Some(s) = self.declare(Declaration {
                                name: self_name,
                                span: self.span(d),
                                kind: SymbolKind::Receiver,
                                mutable: false,
                                ty: instance,
                                owner: Some(f),
                                iteration: false,
                            }) {
                                self.initialized[s.0 as usize] = true;
                                self.out.functions[f.0 as usize].parameters.push(Parameter {
                                    symbol: s,
                                    annotation: None,
                                    default: None,
                                    rest: false,
                                });
                            }
                        }
                        let annotation = annotation.map(|a| self.annotation(a, &c));
                        let t = self.visit(init, &c);
                        if let Some(expected) = annotation {
                            self.require(init, t, expected, "field initializer");
                        }
                        self.set_ty(d, annotation.unwrap_or(if mutable { ANY } else { t }));
                        self.scope = previous;
                    } else if let Some(a) = annotation {
                        self.annotation(a, ctx);
                    }
                }
                _ => (),
            }
        }
        self.class = old_class;
        if let Some(s) = self.out.facts[n.0 as usize].symbol {
            self.initialized[s.0 as usize] = true;
        }
        self.intern_type(Type::Class(n))
    }
    fn import(&mut self, n: NodeId, public: bool, ctx: &Context) {
        if let Some(path) = self.children(n).first().copied() {
            self.import_path(n, path, &[], false, public, ctx);
        }
    }
    fn import_path(
        &mut self,
        root: NodeId,
        n: NodeId,
        prefix: &[String],
        grouped: bool,
        public: bool,
        ctx: &Context,
    ) {
        if !self.budget(n) {
            return;
        }
        let mut path = prefix.to_vec();
        let mut alias = None;
        let mut after_as = false;
        let mut wildcard = false;
        for t in self.tokens(n) {
            match t.kind {
                K::As => after_as = true,
                K::Ident => {
                    let text = self.out.hir.name(t.name.unwrap()).to_owned();
                    if after_as {
                        alias = Some(text);
                    } else {
                        path.push(text);
                    }
                }
                K::Star => wildcard = true,
                _ => (),
            }
        }
        if let Some(group) = self
            .children(n)
            .into_iter()
            .find(|&c| self.kind(c) == HirKind::ImportGroup)
        {
            for child in self.children(group) {
                self.import_path(root, child, &path, true, public, ctx);
            }
            return;
        }
        if self.out.imports.len() >= self.options.limits.max_imports {
            self.error(D::BudgetExceeded, self.span(n), "import budget exceeded");
            self.exhausted = true;
            return;
        }
        let request = ImportRequest {
            from: self.options.module.clone(),
            path,
            kind: if wildcard {
                ImportKind::Wildcard
            } else if grouped {
                ImportKind::Symbol
            } else {
                ImportKind::NamespaceOrSymbol
            },
        };
        match self.resolve(&request) {
            Err(error) => self.error(D::ModuleResolution, self.span(n), error.to_string()),
            Ok(import) => {
                let mut names = Vec::new();
                for ext in import.bindings {
                    let local = alias.as_deref().unwrap_or(&ext.name);
                    let name = self.intern_name(local);
                    let ty = self.external_type(&ext);
                    if let Some(s) = self.declare(Declaration {
                        name,
                        span: self.span(n),
                        kind: SymbolKind::Import,
                        mutable: false,
                        ty,
                        owner: ctx.function,
                        iteration: false,
                    }) {
                        self.initialized[s.0 as usize] = true;
                        self.external.insert(s, ExternalDescriptor::reference(ext));
                        names.push((name, s));
                        if public {
                            self.out.exports.push(s);
                        }
                    }
                }
                self.out.imports.push(Import {
                    node: root,
                    module: import.module,
                    version: import.version,
                    names,
                });
            }
        }
    }
    fn effects(&mut self) {
        if self.exhausted {
            return;
        }
        let mut reverse = vec![Vec::new(); self.out.functions.len()];
        let mut queue = VecDeque::new();
        for (i, f) in self.out.functions.iter().enumerate() {
            if f.known_wait {
                queue.push_back(FunctionId(i as u32));
            }
        }
        for call in std::mem::take(&mut self.calls) {
            if let Some(caller) = call.caller {
                if let Some(callee) = call.callee {
                    if self.out.functions[callee.0 as usize].kind == FunctionKind::Plain {
                        reverse[callee.0 as usize].push(caller);
                    }
                } else if call.effect == CallEffect::MayWait {
                    if !self.out.functions[caller.0 as usize].known_wait {
                        self.out.functions[caller.0 as usize].known_wait = true;
                        queue.push_back(caller);
                    }
                } else if call.effect == CallEffect::Unknown {
                    self.out.functions[caller.0 as usize].dynamic_calls = true;
                }
            }
            // MayWait is a possibility summary. Never reject a NoWait call from
            // this summary: its concrete path may return without external wait.
            if call.no_wait {
                self.check(call.node, RuntimeChecks::NO_WAIT);
            }
            self.validate_call(&call);
        }
        for (i, f) in self.out.functions.iter().enumerate() {
            if f.dynamic_calls {
                queue.push_back(FunctionId(i as u32));
            }
        }
        while let Some(f) = queue.pop_front() {
            let wait = self.out.functions[f.0 as usize].known_wait;
            let dynamic = self.out.functions[f.0 as usize].dynamic_calls;
            for &caller in &reverse[f.0 as usize] {
                let target = &mut self.out.functions[caller.0 as usize];
                if (wait && !target.known_wait) || (dynamic && !target.dynamic_calls) {
                    target.known_wait |= wait;
                    target.dynamic_calls |= dynamic;
                    queue.push_back(caller);
                }
            }
        }
    }
    fn checked_signature(
        &mut self,
        call: &CallSite,
        callee: NodeId,
    ) -> Option<Arc<CheckedSignature>> {
        let descriptor = call
            .named_callee
            .and_then(|name| self.callable_types.get(&name).cloned().flatten())
            .or_else(|| self.external_nodes.get(&callee).cloned());
        let key = if let Some(function) = call.callee {
            SignatureKey::Function(function, call.bound)
        } else {
            SignatureKey::External(Arc::as_ptr(&descriptor.as_ref()?.symbol) as usize)
        };
        if let Some(signature) = self.signatures.get(&key) {
            return Some(signature.clone());
        }
        let count = if let Some(function) = call.callee {
            self.out.functions[function.0 as usize].parameters.len()
        } else {
            descriptor.as_ref()?.signature.as_ref()?.parameters.len()
        };
        let bound = usize::from(call.callee.is_some() && call.bound);
        if count == bound {
            return Some(
                self.empty_signature
                    .get_or_insert_with(|| {
                        Arc::new(CheckedSignature {
                            parameters: Vec::new(),
                            named: HashMap::new(),
                            rest: None,
                            required: Vec::new(),
                        })
                    })
                    .clone(),
            );
        }
        if !self.charge_work(call.node, count.saturating_mul(2)) {
            return None;
        }
        let parameters = if let Some(function) = call.callee {
            self.out.functions[function.0 as usize]
                .parameters
                .iter()
                .skip(usize::from(call.bound))
                .map(|parameter| {
                    let symbol = &self.out.symbols[parameter.symbol.0 as usize];
                    CheckedParameter {
                        name: symbol.name,
                        ty: symbol.ty,
                        required: parameter.default.is_none() && !parameter.rest,
                        rest: parameter.rest,
                    }
                })
                .collect::<Vec<_>>()
        } else {
            descriptor
                .as_ref()?
                .signature
                .as_ref()?
                .parameters
                .iter()
                .map(|parameter| CheckedParameter {
                    name: self.intern_name(&parameter.name),
                    ty: self.hint(&parameter.ty),
                    required: !parameter.default && !parameter.rest,
                    rest: parameter.rest,
                })
                .collect()
        };
        let mut named = HashMap::with_capacity(parameters.len());
        let mut required = Vec::new();
        let mut rest = None;
        for (i, parameter) in parameters.iter().enumerate() {
            if !parameter.rest {
                named.insert(parameter.name, i);
            }
            if parameter.required {
                required.push(i);
            }
            if parameter.rest {
                rest = Some(i);
            }
        }
        let signature = Arc::new(CheckedSignature {
            parameters,
            named,
            required,
            rest,
        });
        self.signatures.insert(key, signature.clone());
        Some(signature)
    }
    fn validate_call(&mut self, call: &CallSite) {
        let Form::Call {
            callee,
            arguments,
            children: trailing,
            ..
        } = self.out.hir.node(call.node).form
        else {
            return;
        };
        let Some(signature) = self.checked_signature(call, callee) else {
            return;
        };
        let mut args = Vec::new();
        for part in arguments.into_iter().chain(trailing) {
            match self.kind(part) {
                HirKind::Arguments => {
                    for arg in self.children(part) {
                        let named = if self.kind(arg) == HirKind::NamedArgument {
                            self.name(arg).and_then(|t| t.name)
                        } else {
                            None
                        };
                        let ty = self.ty(arg);
                        if self.kind(arg) == HirKind::Spread {
                            if let Type::Tuple(elements) = &self.out.types[ty.0 as usize] {
                                let count = elements.len();
                                if !self.charge_work(arg, count) {
                                    return;
                                }
                                let Type::Tuple(elements) = &self.out.types[ty.0 as usize] else {
                                    unreachable!()
                                };
                                args.extend(elements.iter().map(|&ty| (arg, None, ty, false)));
                            } else {
                                args.push((arg, None, ty, true));
                            }
                        } else {
                            args.push((arg, named, ty, false));
                        }
                    }
                }
                HirKind::Children => {
                    let name = self.intern_name("children");
                    args.push((part, Some(name), ARRAY, false));
                }
                _ => (),
            }
        }
        let mut used = HashSet::new();
        let mut position = Some(0usize);
        let mut dynamic_spread = false;
        let mut required_supplied = 0usize;
        for (arg, name, ty, unknown_spread) in args {
            if !self.budget(arg) {
                return;
            }
            if unknown_spread {
                dynamic_spread = true;
                position = None;
                self.check(call.node, RuntimeChecks::CALL);
                continue;
            }
            let index = if let Some(name) = name {
                signature.named.get(&name).copied()
            } else if let Some(i) = position {
                position = Some(i + 1);
                if i < signature.parameters.len() {
                    Some(i)
                } else {
                    signature.rest
                }
            } else {
                // An unknown spread can occupy zero or more positions. The
                // concrete slot and collisions remain runtime binding work.
                self.check(call.node, RuntimeChecks::CALL);
                continue;
            };
            if let Some(i) = index {
                let parameter = &signature.parameters[i];
                if !parameter.rest {
                    if !used.insert(i) {
                        self.error(
                            D::InvalidCall,
                            self.span(arg),
                            "parameter supplied more than once",
                        );
                    } else if parameter.required {
                        required_supplied += 1;
                    }
                    self.require(arg, ty, parameter.ty, "call argument");
                }
            } else {
                self.error(
                    D::InvalidCall,
                    self.span(arg),
                    if name.is_some() {
                        "unknown named argument"
                    } else {
                        "too many positional arguments"
                    },
                );
            }
        }
        if !dynamic_spread && required_supplied < signature.required.len() {
            if !self.charge_work(call.node, signature.required.len()) {
                return;
            }
            for &i in &signature.required {
                if !used.contains(&i) {
                    self.error(
                        D::InvalidCall,
                        self.span(call.node),
                        format!(
                            "missing required argument {}",
                            self.out.hir.name(signature.parameters[i].name)
                        ),
                    );
                    if self.diagnostics.len() >= self.options.limits.max_diagnostics.max(1) {
                        break;
                    }
                }
            }
        }
    }
    fn account(&mut self) {
        let mut bytes = self.out.hir.source.len()
            + self.out.hir.nodes.capacity() * size_of::<HirNode>()
            + self.out.hir.elements.capacity() * size_of::<HirElement>();
        bytes += self.out.hir.names.capacity() * size_of::<String>()
            + self
                .out
                .hir
                .names
                .iter()
                .map(|n| n.capacity())
                .sum::<usize>();
        bytes += self.out.facts.capacity() * size_of::<NodeFacts>()
            + self.out.symbols.capacity() * size_of::<Symbol>()
            + self.out.scopes.capacity() * size_of::<Scope>();
        bytes += self.out.functions.capacity() * size_of::<Function>()
            + self.out.types.capacity() * size_of::<Type>();
        for s in &self.out.scopes {
            bytes += s.symbols.capacity() * size_of::<SymbolId>();
        }
        for f in &self.out.functions {
            bytes += f.captures.capacity() * size_of::<Capture>()
                + f.parameters.capacity() * size_of::<Parameter>();
        }
        for f in &self.out.facts {
            if let Some(c) = &f.control {
                bytes += c.finally.capacity() * size_of::<NodeId>();
            }
        }
        for t in &self.out.types {
            match t {
                Type::Tuple(v) | Type::Union(v) | Type::Callable(v, _) => {
                    bytes += v.capacity() * size_of::<TypeId>()
                }
                _ => (),
            }
        }
        self.out.external = self
            .external
            .iter()
            .map(|(&symbol, descriptor)| ExternalBinding {
                symbol: Some(symbol),
                node: None,
                descriptor: descriptor.symbol.clone(),
                kind: descriptor.kind,
            })
            .chain(
                self.external_nodes
                    .iter()
                    .map(|(&node, descriptor)| ExternalBinding {
                        symbol: None,
                        node: Some(node),
                        descriptor: descriptor.symbol.clone(),
                        kind: descriptor.kind,
                    }),
            )
            .collect();
        self.out.external.sort_by_key(|e| (e.symbol, e.node));
        self.out.stats = AnalysisStats {
            nodes: self.out.hir.nodes.len(),
            symbols: self.out.symbols.len(),
            captures: self.capture_count,
            work_units: self.work,
            retained_bytes: bytes,
            symbol_bytes: self.out.symbols.capacity() * size_of::<Symbol>(),
            capture_bytes: self
                .out
                .functions
                .iter()
                .map(|f| f.captures.capacity() * size_of::<Capture>())
                .sum(),
            imports: self.out.imports.len(),
            signature_builds: self.signatures.len() + usize::from(self.empty_signature.is_some()),
            signature_parameters: self
                .signatures
                .values()
                .map(|signature| signature.parameters.len())
                .sum(),
        };
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
