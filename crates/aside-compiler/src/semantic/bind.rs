use super::*;
use crate::hir::*;
use crate::{DiagnosticCode as D, RelatedRange, SyntaxKind as K, TextRange};
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
#[derive(Clone)]
struct CallSite {
    node: NodeId,
    caller: Option<FunctionId>,
    callee: Option<FunctionId>,
    bound: bool,
    effect: CallEffect,
    no_wait: bool,
}
#[derive(Clone)]
struct PatternName {
    node: NodeId,
    name: NameId,
    span: TextRange,
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
struct Binder<'a> {
    out: AnalyzedModule,
    options: &'a AnalysisOptions,
    resolver: &'a mut dyn ModuleResolver,
    diagnostics: Vec<Diagnostic>,
    exhausted: bool,
    scope: ScopeId,
    maps: Vec<HashMap<NameId, SymbolId>>,
    names: HashMap<String, NameId>,
    types: HashMap<Type, TypeId>,
    initialized: Vec<bool>,
    annotations: HashSet<SymbolId>,
    fn_nodes: HashMap<NodeId, FunctionId>,
    capture_maps: Vec<HashMap<SymbolId, usize>>,
    capture_users: Vec<Vec<(FunctionId, usize)>>,
    external: HashMap<SymbolId, Arc<ExternalSymbol>>,
    external_nodes: HashMap<NodeId, Arc<ExternalSymbol>>,
    calls: Vec<CallSite>,
    forbidden_parameters: HashSet<NameId>,
    analyzed_functions: HashSet<FunctionId>,
    class: Option<(NodeId, bool)>,
    class_members: HashMap<NodeId, HashMap<(NameId, bool), NodeId>>,
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
        calls: Vec::new(),
        forbidden_parameters: HashSet::new(),
        analyzed_functions: HashSet::new(),
        class: None,
        class_members: HashMap::new(),
        class_bases: HashMap::new(),
        derived_functions: HashSet::new(),
        fresh: HashSet::new(),
        work: 0,
        capture_count: 0,
    };
    b.new_scope(None, None, span);
    for host in &options.hosts {
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
            b.external.insert(symbol, Arc::new(host.clone()));
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
        if options.context == AnalysisContext::Generator {
            b.out.functions[id.0 as usize].kind = FunctionKind::Generator;
        }
    }
    b.scope = b.new_scope(Some(ScopeId(0)), ctx.function, span);
    b.block(NodeId(0), &ctx, false);
    b.effects();
    b.account();
    Analysis {
        module: if b.diagnostics.is_empty() && !b.exhausted {
            Some(b.out)
        } else {
            None
        },
        diagnostics: b.diagnostics,
        exhausted: b.exhausted,
    }
}
impl Binder<'_> {
    fn kind(&self, n: NodeId) -> HirKind {
        self.out.hir.node(n).kind
    }
    fn span(&self, n: NodeId) -> TextRange {
        self.out.hir.node(n).span
    }
    fn children(&self, n: NodeId) -> Vec<NodeId> {
        self.out.hir.children(n).collect()
    }
    fn tokens(&self, n: NodeId) -> Vec<HirToken> {
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
        if self.exhausted {
            return false;
        }
        self.work += 1;
        if self.work > self.options.limits.max_work {
            self.exhausted = true;
            self.error(
                D::BudgetExceeded,
                self.span(n),
                "semantic work budget exceeded",
            );
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
    fn hint(&mut self, t: &TypeHint) -> TypeId {
        match t {
            TypeHint::Any => ANY,
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
        let Declaration {
            name,
            span,
            kind,
            mutable,
            ty,
            owner,
            iteration,
        } = declaration;
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
        self.out.symbols.push(Symbol {
            name,
            declaration: span,
            scope: self.scope,
            owner,
            kind,
            mutable,
            ty,
            storage: if owner.is_none() && kind != SymbolKind::Host {
                Storage::Module
            } else {
                Storage::Local
            },
            per_iteration: iteration,
            initialization: match kind {
                SymbolKind::Function => Initialization::ScopeEntry,
                SymbolKind::Parameter | SymbolKind::Receiver | SymbolKind::Super => {
                    Initialization::Parameter
                }
                SymbolKind::Host | SymbolKind::Import => Initialization::External,
                _ => Initialization::Declaration,
            },
        });
        self.initialized.push(false);
        self.capture_users.push(Vec::new());
        self.maps[self.scope.0 as usize].insert(name, id);
        self.out.scopes[self.scope.0 as usize].symbols.push(id);
        Some(id)
    }
    fn lookup(&self, name: NameId) -> Option<SymbolId> {
        let mut s = Some(self.scope);
        while let Some(id) = s {
            if let Some(&symbol) = self.maps[id.0 as usize].get(&name) {
                return Some(symbol);
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
        let previous = self.scope;
        if new_scope {
            self.scope = self.new_scope(Some(previous), ctx.function, self.span(n));
        }
        self.out.facts[n.0 as usize].scope = Some(self.scope);
        let children = self.children(n);
        for &child in &children {
            self.predeclare(child, ctx);
        }
        // Imports are linked before visiting initializers, independent of textual order.
        for &child in &children {
            let d = self.unwrap(child);
            if self.kind(d) == HirKind::Import {
                self.import(d, self.modifier(child, K::Pub), ctx);
            }
        }
        // Signatures and definition environments are available at block entry,
        // including forward fn references. Ordinary lets remain uninitialized.
        for &child in &children {
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
        let body = self
            .children(n)
            .into_iter()
            .find(|&c| self.kind(c) == HirKind::Block)
            .unwrap_or_else(|| self.children(n).last().copied().unwrap_or(n));
        let kind = if self.token(n, K::Async).is_some() {
            FunctionKind::Async
        } else if self.token(n, K::Gen).is_some() {
            FunctionKind::Generator
        } else {
            FunctionKind::Plain
        };
        self.out.functions.push(Function {
            node: n,
            parent: ctx.function,
            kind,
            receiver,
            role: FunctionRole::Body,
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
    fn function(&mut self, n: NodeId, ctx: &Context, receiver: Receiver) -> TypeId {
        let f = self.make_function(n, ctx, receiver);
        self.out.functions[f.0 as usize].receiver = receiver;
        let ty = self.intern_type(Type::Function(f));
        if !self.analyzed_functions.insert(f) {
            return self.set_ty(n, ty);
        }
        let previous = self.scope;
        self.scope = self.new_scope(Some(previous), Some(f), self.span(n));
        self.out.facts[n.0 as usize].scope = Some(self.scope);
        let kind = self.out.functions[f.0 as usize].kind;
        let method_name = self
            .name(n)
            .map(|t| self.out.hir.name(t.name.unwrap()).to_owned());
        let restricted_method = receiver == Receiver::ExplicitSelf
            && method_name
                .as_deref()
                .is_some_and(|s| s == "init" || (s.starts_with('_') && s != "_call"));
        // Function definitions reset dynamic execution context; constructing a
        // closure in children/finally does not constrain its eventual body.
        let mut inner = Context {
            function: Some(f),
            generator: kind == FunctionKind::Generator,
            no_wait: kind == FunctionKind::Generator
                || restricted_method
                || self.derived_functions.contains(&n),
            derived: self.derived_functions.contains(&n),
            ..Default::default()
        };
        let old_forbidden = std::mem::take(&mut self.forbidden_parameters);
        let params = self
            .children(n)
            .into_iter()
            .find(|&c| self.kind(c) == HirKind::Parameters)
            .map(|p| self.children(p))
            .unwrap_or_default();
        self.forbidden_parameters = params
            .iter()
            .filter_map(|&p| self.name(p).and_then(|t| t.name))
            .collect();
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
            let Some(token) = self.name(p) else {
                continue;
            };
            let name = token.name.unwrap();
            let children = self.children(p);
            let annotation = children
                .iter()
                .copied()
                .find(|&c| is_type(self.kind(c)))
                .map(|a| self.annotation(a, &inner));
            let default = children.iter().copied().find(|&c| !is_type(self.kind(c)));
            let mut default_ctx = inner.clone();
            default_ctx.default = true;
            default_ctx.no_wait = true;
            if let Some(default) = default {
                let actual = self.visit(default, &default_ctx);
                if let Some(expected) = annotation {
                    self.require(default, actual, expected, "parameter default");
                }
            }
            self.forbidden_parameters.remove(&name);
            let mut pty = annotation.unwrap_or(ANY);
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
        self.forbidden_parameters.clear();
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
        if let Some(ret) = self
            .children(n)
            .into_iter()
            .find(|&c| is_type(self.kind(c)))
        {
            let result = self.annotation(ret, &inner);
            self.out.functions[f.0 as usize].return_type = result;
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
        inner.no_wait = false;
        self.forbidden_parameters = old_forbidden;
        self.scope = previous;
        self.set_ty(n, ty)
    }
    fn read(&mut self, n: NodeId, token: HirToken, ctx: &Context, write: bool) -> TypeId {
        let name = token.name.unwrap();
        if ctx.default && self.forbidden_parameters.contains(&name) {
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
        let symbol = self.out.symbols[s.0 as usize].clone();
        if symbol.kind == SymbolKind::Import {
            self.check(n, RuntimeChecks::INITIALIZED);
        }
        if !self.initialized[s.0 as usize] {
            if symbol.owner == ctx.function {
                self.error(
                    D::UninitializedBinding,
                    token.span,
                    "binding is not initialized",
                );
            } else {
                self.check(n, RuntimeChecks::INITIALIZED);
            }
        }
        if write && ctx.derived && symbol.owner != ctx.function && !ctx.conditional {
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
        if symbol.kind == SymbolKind::Host || symbol.owner.is_none() {
            return;
        }
        let mut function = ctx.function;
        while let Some(f) = function {
            if Some(f) == symbol.owner {
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
                if needs_cell {
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
        let children = self.children(n);
        let result = match self.kind(n) {
            HirKind::NamedType => {
                let text = self
                    .tokens(n)
                    .iter()
                    .map(|t| self.out.hir.text(t.span))
                    .collect::<String>();
                match text.as_str() {
                    "any" => ANY,
                    "null" => NULL,
                    "bool" => BOOL,
                    "int" => INT,
                    "float" => FLOAT,
                    "string" => STRING,
                    "array" => ARRAY,
                    "table" | "Map" => TABLE,
                    "Future" => FUTURE,
                    "generator" => GENERATOR,
                    "closure" => self.intern_type(Type::Callable(Vec::new(), ANY)),
                    _ => {
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
                                // Standard value categories are recognized without implicit variables.
                                if matches!(
                                    text.as_str(),
                                    "Bytes"
                                        | "TextBuffer"
                                        | "ByteBuffer"
                                        | "Set"
                                        | "Range"
                                        | "Iterator"
                                        | "AsyncIterator"
                                        | "thread"
                                        | "weakref"
                                        | "class"
                                        | "instance"
                                ) {
                                    self.intern_type(Type::Named(name))
                                } else {
                                    self.error(
                                        D::InvalidType,
                                        t.span,
                                        format!("unknown type {text}"),
                                    );
                                    ANY
                                }
                            }
                        } else {
                            ANY
                        }
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
            (Type::Named(_) | Type::Instance(_), Type::Named(_) | Type::Instance(_)) => true,
            _ => false,
        }
    }
    fn require(&mut self, n: NodeId, actual: TypeId, expected: TypeId, label: &str) {
        if !self.compatible(actual, expected) {
            self.error(
                D::TypeMismatch,
                self.span(n),
                format!(
                    "{label}: {:?} cannot satisfy {:?}",
                    self.out.types[actual.0 as usize], self.out.types[expected.0 as usize]
                ),
            );
        } else if actual == ANY || actual != expected {
            self.check(n, RuntimeChecks::TYPE);
        }
    }
    fn pattern_names(&mut self, n: NodeId) -> Vec<PatternName> {
        if !self.budget(n) {
            return Vec::new();
        }
        match self.kind(n) {
            HirKind::NamePattern | HirKind::RestPattern => self
                .name(n)
                .filter(|t| self.out.hir.name(t.name.unwrap()) != "_")
                .map(|t| {
                    vec![PatternName {
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
                        vec![PatternName {
                            node: n,
                            name: t.name.unwrap(),
                            span: t.span,
                        }]
                    })
                    .unwrap_or_default()
            }
            HirKind::OrPattern => {
                let branches = self.children(n);
                let mut result = Vec::new();
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
                let mut result = Vec::new();
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
        declare: bool,
        mutable: bool,
    ) {
        self.pattern_defaults(n, ctx);
        let names = self.pattern_names(n);
        for name in names {
            let symbol = if declare {
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
        self.pattern_type(n, value);
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
    fn pattern_type(&mut self, n: NodeId, value: TypeId) {
        match self.kind(n) {
            HirKind::TuplePattern => {
                if let Type::Tuple(ts) = self.out.types[value.0 as usize].clone() {
                    let children = self.children(n);
                    let rest = children
                        .last()
                        .is_some_and(|&c| self.kind(c) == HirKind::RestPattern);
                    if (!rest && children.len() != ts.len())
                        || (rest && children.len() - 1 > ts.len())
                    {
                        self.error(
                            D::TypeMismatch,
                            self.span(n),
                            "tuple pattern has incompatible arity",
                        );
                    }
                    for (i, c) in children.into_iter().enumerate() {
                        if self.kind(c) == HirKind::RestPattern {
                            let t = self
                                .intern_type(Type::Tuple(ts.get(i..).unwrap_or_default().to_vec()));
                            self.pattern_type(c, t);
                        } else {
                            self.pattern_type(c, ts.get(i).copied().unwrap_or(ANY));
                        }
                    }
                } else if value != ANY {
                    let expected = self.intern_tuple_unknown(n);
                    self.require(n, value, expected, "tuple pattern");
                }
            }
            HirKind::ArrayPattern => {
                self.require(n, value, ARRAY, "array pattern");
                for c in self.children(n) {
                    self.pattern_type(
                        c,
                        if self.kind(c) == HirKind::RestPattern {
                            ARRAY
                        } else {
                            ANY
                        },
                    );
                }
            }
            HirKind::NamePattern | HirKind::RestPattern => {
                if let Some(s) = self.out.facts[n.0 as usize].symbol
                    && !self.out.symbols[s.0 as usize].mutable
                {
                    self.out.symbols[s.0 as usize].ty = value;
                }
            }
            HirKind::GroupPattern | HirKind::TypePattern | HirKind::OrPattern => {
                for c in self.children(n) {
                    if is_pattern(self.kind(c)) {
                        self.pattern_type(c, value);
                    }
                }
            }
            _ => (),
        }
    }
    fn intern_tuple_unknown(&mut self, n: NodeId) -> TypeId {
        let len = self.children(n).len();
        self.intern_type(Type::Tuple(vec![ANY; len]))
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
                        false,
                        self.modifier(n, K::Mut),
                    );
                    if let Some(init) = init
                        && self.is_fresh(init)
                    {
                        for name in self.pattern_names(p) {
                            if let Some(s) = self.lookup(name.name) {
                                self.fresh.insert(s);
                            }
                        }
                    }
                    if let Some(expected) = annotation {
                        for name in self.pattern_names(p) {
                            if let Some(s) = self.lookup(name.name) {
                                self.annotations.insert(s);
                                if self.kind(p) == H::NamePattern {
                                    self.out.symbols[s.0 as usize].ty = expected;
                                }
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
            H::Binary | H::Range => self.binary(n, &children, ctx),
            H::Assignment => {
                self.assignment(n, ctx);
                NULL
            }
            H::Member => self.member(n, &children, ctx),
            H::Index => {
                let object = children.first().map_or(ANY, |&c| self.visit(c, ctx));
                let index = children.get(1).map_or(ANY, |&c| self.visit(c, ctx));
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
            H::Call => self.call(n, &children, ctx),
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
                if ctx.no_wait && !ctx.conditional {
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
    fn binary(&mut self, n: NodeId, children: &[NodeId], ctx: &Context) -> TypeId {
        let a = children.first().map_or(ANY, |&c| self.visit(c, ctx));
        let b = children.get(1).map_or(ANY, |&c| self.visit(c, ctx));
        let op = self.tokens(n).first().map(|t| t.kind);
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
                } else if a == b {
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
        if let Some(s) = self.out.facts[target.0 as usize].symbol
            && self.annotations.contains(&s)
            && let Some(rhs) = rhs
        {
            self.require(rhs, value, self.out.symbols[s.0 as usize].ty, "assignment");
        }
        if is_pattern(self.kind(target)) {
            self.pattern_type(target, value);
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
                self.check(n, RuntimeChecks::MUTABILITY);
                if ctx.derived {
                    self.check(n, RuntimeChecks::WRITE_EFFECT);
                    let object = self.children(n).first().copied();
                    if let Some(symbol) = object.and_then(|o| self.root_symbol(o)) {
                        if self.out.symbols[symbol.0 as usize].owner != ctx.function
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
                if self.kind(n) == HirKind::OrPattern {
                    self.pattern_names(n);
                }
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
    fn member(&mut self, n: NodeId, children: &[NodeId], ctx: &Context) -> TypeId {
        let object = children.first().map_or(ANY, |&c| self.visit(c, ctx));
        let key = self
            .name(n)
            .map(|t| self.out.hir.name(t.name.unwrap()).to_owned());
        if let (Some(&base), Some(key)) = (children.first(), key) {
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
                match self.resolver.resolve(&request) {
                    Ok(mut result) if result.version == namespace.version => {
                        if let Some(symbol) = result.bindings.pop() {
                            let ty = self.hint(&symbol.ty);
                            self.external_nodes.insert(n, Arc::new(symbol));
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
                if let Some(d) = self.find_class_member(class, name, !instance)
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
        for _ in 0..self.options.limits.max_depth.clamp(1, 128) {
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
    fn call(&mut self, n: NodeId, _children: &[NodeId], ctx: &Context) -> TypeId {
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
        let callee = Some(target);
        let ty = callee.map_or(ANY, |c| self.visit(c, ctx));
        let descriptor = self.external_nodes.get(&target).cloned();
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
        if let Some(intrinsic) = descriptor.as_ref().and_then(|d| d.intrinsic) {
            self.out.facts[n.0 as usize].intrinsic = Some(intrinsic);
            if intrinsic == Intrinsic::Bind {
                let value = arguments
                    .and_then(|args| self.children(args).first().copied())
                    .and_then(|a| self.children(a).first().copied());
                if let Some(value) = value {
                    if let Form::Member { object, .. } | Form::Index { object, .. } =
                        self.out.hir.node(value).form
                    {
                        if self.ty(object) == TABLE {
                            self.error(
                                D::TypeMismatch,
                                self.span(value),
                                "ordinary table slots are not observable properties",
                            );
                        }
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
        let external = callee.and_then(|c| self.external_nodes.get(&c)).cloned();
        let effect = external
            .as_ref()
            .and_then(|e| e.signature.as_ref())
            .map_or(CallEffect::Unknown, |s| s.effect);
        self.calls.push(CallSite {
            node: n,
            caller: ctx.function,
            callee: function,
            bound,
            effect,
            no_wait: ctx.no_wait,
        });
        self.check(n, RuntimeChecks::CALL);
        if ctx.no_wait {
            self.check(n, RuntimeChecks::NO_WAIT);
        }
        if let Some(external) = external
            && let Some(signature) = &external.signature
        {
            return self.hint(&signature.result);
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
        let children = self.children(n);
        let pattern = children.iter().copied().find(|&c| is_pattern(self.kind(c)));
        let condition = children.iter().copied().find(|&c| {
            !is_pattern(self.kind(c))
                && self.kind(c) != HirKind::Block
                && self.kind(c) != HirKind::If
        });
        let value = condition.map_or(ANY, |c| self.visit(c, ctx));
        if pattern.is_none()
            && let Some(c) = condition
        {
            self.require(c, value, BOOL, "condition");
        }
        let previous = self.scope;
        let mut inner = ctx.clone();
        if is_loop {
            inner.loops.push((n, None, ctx.finally.len()));
            inner.iteration = true;
        }
        self.scope = self.new_scope(Some(previous), ctx.function, self.span(n));
        if let Some(p) = pattern {
            self.bind_pattern(p, value, &inner, true, false);
        }
        let bodies: Vec<_> = children
            .iter()
            .copied()
            .filter(|&c| matches!(self.kind(c), HirKind::Block | HirKind::If))
            .collect();
        let constant = condition.and_then(|c| self.constant_bool(c));
        inner.conditional |= constant != Some(true);
        let yes = bodies.first().map_or(NULL, |&c| self.visit(c, &inner));
        self.scope = previous;
        let mut otherwise = ctx.clone();
        otherwise.conditional |= constant != Some(false);
        let no = bodies.get(1).map_or(NULL, |&c| self.visit(c, &otherwise));
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
            // condition's own anonymous loop would hide the explicit label only
            // for unlabelled jumps, which still targets this same node.
            self.condition(n, &c, true);
            return NULL;
        }
        let children = self.children(n);
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
        inner.conditional = true;
        let source = children
            .iter()
            .copied()
            .find(|&c| !is_pattern(self.kind(c)) && self.kind(c) != HirKind::Block);
        let value = source.map_or(ANY, |c| self.visit(c, ctx));
        if self.token(n, K::Await).is_some() {
            if ctx.no_wait && !ctx.conditional {
                self.error(
                    D::NoWait,
                    self.span(n),
                    "for await is forbidden in this execution context",
                );
            }
            if let Some(f) = ctx.function {
                self.out.functions[f.0 as usize].known_wait = true;
            }
        }
        let previous = self.scope;
        self.scope = self.new_scope(Some(previous), ctx.function, self.span(n));
        if let Some(p) = children.iter().copied().find(|&c| is_pattern(self.kind(c))) {
            let item = match &self.out.types[value.0 as usize] {
                Type::Tuple(ts) => ts
                    .iter()
                    .copied()
                    .reduce(|a, b| if a == b { a } else { ANY })
                    .unwrap_or(ANY),
                _ => ANY,
            };
            self.bind_pattern(p, item, &inner, true, false);
        }
        for c in children {
            if self.kind(c) == HirKind::Block {
                self.visit(c, &inner);
            }
        }
        self.scope = previous;
        NULL
    }
    fn match_node(&mut self, n: NodeId, children: &[NodeId], ctx: &Context) -> TypeId {
        let value = children.first().map_or(ANY, |&c| self.visit(c, ctx));
        let mut result = None;
        for &arm in children.iter().skip(1) {
            let previous = self.scope;
            self.scope = self.new_scope(Some(previous), ctx.function, self.span(arm));
            let parts = self.children(arm);
            if let Some(&p) = parts.first() {
                self.bind_pattern(p, value, ctx, true, false);
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
    fn falls_through(&self, n: NodeId) -> bool {
        match self.kind(n) {
            HirKind::Return | HirKind::Throw | HirKind::Break | HirKind::Continue => false,
            HirKind::Block | HirKind::Module | HirKind::ExpressionStatement => self
                .out
                .hir
                .children(n)
                .last()
                .is_none_or(|c| self.falls_through(c)),
            HirKind::If => {
                let branches: Vec<_> = self
                    .out
                    .hir
                    .children(n)
                    .filter(|&c| matches!(self.kind(c), HirKind::Block | HirKind::If))
                    .collect();
                branches.len() < 2 || branches.into_iter().any(|b| self.falls_through(b))
            }
            _ => true,
        }
    }
    fn class_node(&mut self, n: NodeId, children: &[NodeId], ctx: &Context) -> TypeId {
        let base = children
            .iter()
            .copied()
            .find(|&c| !matches!(self.kind(self.unwrap(c)), HirKind::Let | HirKind::Function));
        if let Some(base) = base {
            let value = self.visit(base, ctx);
            if let Type::Class(parent) = self.out.types[value.0 as usize] {
                self.class_bases.insert(n, parent);
            }

            if matches!(self.out.types[value.0 as usize],Type::Class(c)if c==n) {
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
        self.class = Some((n, base.is_some()));
        let mut seen = HashSet::new();
        for &member in children {
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
            }
        }
        for &member in children {
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
                        ..
                    } = self.out.hir.node(d).form
                    else {
                        continue;
                    };
                    if let Some(init) = initializer {
                        let previous = self.scope;
                        let f = self.make_function(d, ctx, Receiver::None);
                        self.out.functions[f.0 as usize].role = FunctionRole::FieldInitializer;
                        self.out.functions[f.0 as usize].body = init;
                        self.scope = self.new_scope(Some(previous), Some(f), self.span(d));
                        let c = Context {
                            function: Some(f),
                            no_wait: true,
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
        match self.resolver.resolve(&request) {
            Err(error) => self.error(D::ModuleResolution, self.span(n), error.to_string()),
            Ok(import) => {
                let mut names = Vec::new();
                for ext in import.bindings {
                    let local = alias.as_deref().unwrap_or(&ext.name);
                    let name = self.intern_name(local);
                    let ty = self.hint(&ext.ty);
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
                        self.external.insert(s, Arc::new(ext));
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
        let mut reverse = vec![Vec::new(); self.out.functions.len()];
        let mut queue = VecDeque::new();
        for (i, f) in self.out.functions.iter().enumerate() {
            if f.known_wait {
                queue.push_back(FunctionId(i as u32));
            }
        }
        for call in self.calls.clone() {
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
        let signature = if let Some(f) = call.callee {
            let f = &self.out.functions[f.0 as usize];
            let bound = call.bound;
            Some((
                f.parameters
                    .iter()
                    .skip(usize::from(bound))
                    .map(|p| {
                        let symbol = &self.out.symbols[p.symbol.0 as usize];
                        (symbol.name, symbol.ty, p.default.is_some(), p.rest)
                    })
                    .collect::<Vec<_>>(),
                f.return_type,
            ))
        } else {
            self.external_nodes
                .get(&callee)
                .and_then(|e| e.signature.clone())
                .map(|sig| {
                    let p = sig
                        .parameters
                        .iter()
                        .map(|p| {
                            let name = self.intern_name(&p.name);
                            let ty = self.hint(&p.ty);
                            (name, ty, p.default, p.rest)
                        })
                        .collect();
                    (p, self.hint(&sig.result))
                })
        };
        let Some((params, _)) = signature else {
            return;
        };
        let mut args = Vec::new();
        let mut dynamic_spread = false;
        for part in arguments.into_iter().chain(trailing) {
            match self.kind(part) {
                HirKind::Arguments => {
                    for arg in self.children(part) {
                        let named = if self.kind(arg) == HirKind::NamedArgument {
                            self.name(arg).and_then(|t| t.name)
                        } else {
                            None
                        };
                        if self.kind(arg) == HirKind::Spread {
                            dynamic_spread = true;
                        }
                        args.push((arg, named, self.ty(arg)));
                    }
                }
                HirKind::Children => {
                    let name = self.intern_name("children");
                    args.push((part, Some(name), ARRAY));
                }
                _ => (),
            }
        }
        let mut used = HashSet::new();
        let mut position = 0;
        let rest = params.iter().position(|p| p.3);
        for (arg, name, ty) in args {
            let index = if let Some(name) = name {
                params.iter().position(|p| p.0 == name)
            } else {
                let i = position;
                position += 1;
                if i < params.len() { Some(i) } else { rest }
            };
            if let Some(i) = index {
                if !params[i].3 && !used.insert(i) {
                    self.error(
                        D::InvalidCall,
                        self.span(arg),
                        "parameter supplied more than once",
                    );
                }
                if !dynamic_spread && !params[i].3 {
                    self.require(arg, ty, params[i].1, "call argument");
                }
            } else if !dynamic_spread {
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
        if !dynamic_spread {
            for (i, p) in params.iter().enumerate() {
                if !p.2 && !p.3 && !used.contains(&i) {
                    self.error(
                        D::InvalidCall,
                        self.span(call.node),
                        format!("missing required argument {}", self.out.hir.name(p.0)),
                    );
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
                descriptor: descriptor.clone(),
            })
            .chain(
                self.external_nodes
                    .iter()
                    .map(|(&node, descriptor)| ExternalBinding {
                        symbol: None,
                        node: Some(node),
                        descriptor: descriptor.clone(),
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
