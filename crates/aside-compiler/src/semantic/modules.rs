//! Versioned, injectable module interfaces. No IO or path policy lives here.
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeHint {
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
    Tuple(Vec<TypeHint>),
    Union(Vec<TypeHint>),
    Callable(Vec<TypeHint>, Box<TypeHint>),
    Named(String),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEffect {
    NoWait,
    MayWait,
    Unknown,
}
#[derive(Debug, Clone)]
pub struct ExternalParameter {
    pub name: String,
    pub ty: TypeHint,
    pub mutable: bool,
    pub default: bool,
    pub rest: bool,
}
#[derive(Debug, Clone)]
pub struct ExternalSignature {
    pub parameters: Vec<ExternalParameter>,
    pub result: TypeHint,
    pub effect: CallEffect,
    pub init_safe: Option<bool>,
    pub derived_safe: Option<bool>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intrinsic {
    Bind,
    Derive,
}
#[derive(Debug, Clone)]
pub struct NamespaceRef {
    pub module: String,
    pub version: String,
}
/// Canonical declaration identity supplied by a module resolver or the single
/// trusted native registry. A namespace references the module with export=None.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolReference {
    pub module: String,
    pub version: String,
    pub export: Option<String>,
}
/// Resolution path preserves reexport identities without changing declaration
/// identity or copying a whole provenance chain for each alias/read.
#[derive(Debug, Clone)]
pub struct ResolutionSource {
    pub reference: Arc<SymbolReference>,
    pub previous: Option<Arc<ResolutionSource>>,
}
impl Drop for ResolutionSource {
    fn drop(&mut self) {
        // Detach before releasing the last Arc. Every uniquely owned node is
        // dropped with previous=None; a shared tail remains owned by its users.
        // Arc::into_inner also handles simultaneous final releases correctly.
        let mut previous = self.previous.take();
        while let Some(source) = previous {
            let Some(mut source) = Arc::into_inner(source) else {
                break;
            };
            previous = source.previous.take();
        }
    }
}
#[derive(Debug, Clone)]
pub struct ExternalSymbol {
    pub name: String,
    pub reference: Option<Arc<SymbolReference>>,
    pub resolved_from: Option<Arc<ResolutionSource>>,
    pub ty: TypeHint,
    pub signature: Option<ExternalSignature>,
    pub namespace: Option<NamespaceRef>,
    pub intrinsic: Option<Intrinsic>,
    /// A discarded call result is a source error; explicit wildcard bindings
    /// and lexical children collection consume the result.
    pub must_use_result: bool,
}
impl ExternalSymbol {
    /// Use the exact registration identity; no ABI/capability/default registry
    /// is synthesized by the analyzer. Missing identity stays explicit as None.
    pub fn with_reference(mut self, reference: SymbolReference) -> Self {
        self.reference = Some(Arc::new(reference));
        self
    }
    pub(super) fn resolved_at(&mut self, module: &str, version: &str, namespace: bool) {
        let reference = SymbolReference {
            module: module.into(),
            version: version.into(),
            export: (!namespace).then(|| self.name.clone()),
        };
        if self.reference.is_none() {
            self.reference = Some(Arc::new(reference.clone()));
        }
        if self
            .resolved_from
            .as_ref()
            .is_some_and(|r| *r.reference == reference)
        {
            return;
        }
        let reference = if self.reference.as_ref().is_some_and(|r| **r == reference) {
            self.reference.as_ref().unwrap().clone()
        } else {
            Arc::new(reference)
        };
        self.resolved_from = Some(Arc::new(ResolutionSource {
            reference,
            previous: self.resolved_from.take(),
        }));
    }

    pub fn intrinsic(name: impl Into<String>, kind: Intrinsic) -> Self {
        Self {
            intrinsic: Some(kind),
            ..Self::dynamic(name)
        }
    }
    pub fn dynamic(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            reference: None,
            resolved_from: None,
            ty: TypeHint::Any,
            signature: None,
            namespace: None,
            intrinsic: None,
            must_use_result: false,
        }
    }
    pub fn value(name: impl Into<String>, ty: TypeHint) -> Self {
        Self {
            name: name.into(),
            reference: None,
            resolved_from: None,
            ty,
            signature: None,
            namespace: None,
            intrinsic: None,
            must_use_result: false,
        }
    }
    pub fn function(name: impl Into<String>, signature: ExternalSignature) -> Self {
        Self {
            name: name.into(),
            reference: None,
            resolved_from: None,
            ty: TypeHint::Any,
            signature: Some(signature),
            namespace: None,
            intrinsic: None,
            must_use_result: false,
        }
    }
}
#[derive(Debug, Clone)]
pub struct ModuleInterface {
    pub canonical_path: String,
    pub version: String,
    /// Only public exports, never private environment slots.
    pub exports: Vec<ExternalSymbol>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    NamespaceOrSymbol,
    Symbol,
    Wildcard,
}
#[derive(Debug, Clone)]
pub struct ImportRequest {
    pub from: String,
    pub path: Vec<String>,
    pub kind: ImportKind,
}
#[derive(Debug, Clone)]
pub struct ResolvedImport {
    pub module: String,
    pub version: String,
    pub bindings: Vec<ExternalSymbol>,
}
impl ResolvedImport {
    pub(super) fn retain_references(mut self) -> Self {
        for symbol in &mut self.bindings {
            // A resolver may already have normalized the returned interface.
            // Preserve that hop verbatim: inferring its kind again from the
            // presence of a previous hop can turn a module into a fake export.
            if symbol.resolved_from.as_ref().is_some_and(|source| {
                source.reference.module == self.module && source.reference.version == self.version
            }) {
                continue;
            }
            let namespace = symbol
                .namespace
                .as_ref()
                .is_some_and(|n| n.module == self.module && n.version == self.version)
                && symbol.resolved_from.is_none()
                && symbol.reference.as_ref().is_none_or(|r| r.export.is_none());
            symbol.resolved_at(&self.module, &self.version, namespace);
        }
        self
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError(pub String);
impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ResolveError {}
pub trait ModuleResolver {
    fn canonical_request<'a>(
        &self,
        request: &'a ImportRequest,
    ) -> std::borrow::Cow<'a, ImportRequest> {
        std::borrow::Cow::Borrowed(request)
    }
    /// Resolver owns manifest/version/path/capability policy. A Symbol request
    /// (explicit grouped import) never silently becomes a namespace.
    fn resolve(&mut self, request: &ImportRequest) -> Result<ResolvedImport, ResolveError>;
}
#[derive(Debug, Default)]
pub struct NoModules;
impl ModuleResolver for NoModules {
    fn resolve(&mut self, request: &ImportRequest) -> Result<ResolvedImport, ResolveError> {
        Err(ResolveError(format!(
            "no resolver for {}",
            request.path.join("::")
        )))
    }
}
/// In-memory interfaces for embedders/tests; production adapters can implement
/// the same trait using project manifests without changing the analyzer.
#[derive(Debug, Default, Clone)]
pub struct MemoryResolver {
    pub modules: BTreeMap<String, ModuleInterface>,
    pub aliases: BTreeMap<String, String>,
}
impl MemoryResolver {
    pub fn insert(&mut self, mut module: ModuleInterface) {
        for symbol in &mut module.exports {
            symbol.resolved_at(&module.canonical_path, &module.version, false);
        }
        self.modules.insert(module.canonical_path.clone(), module);
    }
    fn candidates(
        &self,
        request: &ImportRequest,
    ) -> (Option<&ModuleInterface>, Option<&ModuleInterface>) {
        let namespace = (request.kind != ImportKind::Symbol)
            .then(|| self.modules.get(&request.path.join("::")))
            .flatten();
        let symbol_module = (request.kind != ImportKind::Wildcard)
            .then(|| {
                request
                    .path
                    .split_last()
                    .and_then(|(_, prefix)| self.modules.get(&prefix.join("::")))
            })
            .flatten();
        (namespace, symbol_module)
    }
    pub(super) fn dependency_modules(&self, request: &ImportRequest) -> Vec<String> {
        let (namespace, symbol_module) = self.candidates(request);
        namespace
            .into_iter()
            .chain(symbol_module)
            .map(|module| module.canonical_path.clone())
            .collect()
    }
}
pub fn canonical_import<'a>(
    request: &'a ImportRequest,
    aliases: &BTreeMap<String, String>,
) -> std::borrow::Cow<'a, ImportRequest> {
    if aliases.is_empty() {
        return std::borrow::Cow::Borrowed(request);
    }
    let full = request.path.join("::");
    let mut end = full.len();
    loop {
        if let Some(canonical) = aliases
            .get(&full[..end])
            .filter(|canonical| *canonical != &full[..end])
        {
            let prefix = full[..end].split("::").count();
            let mut normalized = request.clone();
            normalized.path = canonical
                .split("::")
                .map(str::to_owned)
                .chain(request.path[prefix..].iter().cloned())
                .collect();
            return std::borrow::Cow::Owned(normalized);
        }
        let Some(prefix) = full[..end].rfind("::") else {
            break;
        };
        end = prefix;
    }
    std::borrow::Cow::Borrowed(request)
}
impl ModuleResolver for MemoryResolver {
    fn canonical_request<'a>(
        &self,
        request: &'a ImportRequest,
    ) -> std::borrow::Cow<'a, ImportRequest> {
        canonical_import(request, &self.aliases)
    }
    fn resolve(&mut self, r: &ImportRequest) -> Result<ResolvedImport, ResolveError> {
        let r = self.canonical_request(r);
        let full = r.path.join("::");
        let (namespace, symbol_module) = self.candidates(&r);
        let symbol = r.path.last().and_then(|last| {
            let module = symbol_module?;
            module
                .exports
                .iter()
                .find(|e| &e.name == last)
                .map(|e| (module, e))
        });
        if r.kind == ImportKind::NamespaceOrSymbol && namespace.is_some() && symbol.is_some() {
            return Err(ResolveError(format!(
                "{full} names both a module and symbol; select symbol with a grouped import"
            )));
        }
        match r.kind {
            ImportKind::Wildcard => namespace.map(|m| ResolvedImport {
                module: m.canonical_path.clone(),
                version: m.version.clone(),
                bindings: m.exports.clone(),
            }),
            ImportKind::Symbol => symbol.map(|(m, s)| ResolvedImport {
                module: m.canonical_path.clone(),
                version: m.version.clone(),
                bindings: vec![s.clone()],
            }),
            ImportKind::NamespaceOrSymbol => namespace
                .map(|m| ResolvedImport {
                    module: m.canonical_path.clone(),
                    version: m.version.clone(),
                    bindings: vec![ExternalSymbol {
                        name: r.path.last().cloned().unwrap_or_default(),
                        reference: None,
                        resolved_from: None,
                        ty: TypeHint::Table,
                        signature: None,
                        intrinsic: None,
                        must_use_result: false,
                        namespace: Some(NamespaceRef {
                            module: m.canonical_path.clone(),
                            version: m.version.clone(),
                        }),
                    }],
                })
                .or_else(|| {
                    symbol.map(|(m, s)| ResolvedImport {
                        module: m.canonical_path.clone(),
                        version: m.version.clone(),
                        bindings: vec![s.clone()],
                    })
                }),
        }
        .map(ResolvedImport::retain_references)
        .ok_or_else(|| ResolveError(format!("module or public symbol {full} was not found")))
    }
}
