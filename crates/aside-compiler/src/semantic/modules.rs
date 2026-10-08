//! Versioned, injectable module interfaces. No IO or path policy lives here.
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeHint {
    Any,
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
#[derive(Debug, Clone)]
pub struct ExternalSymbol {
    pub name: String,
    pub ty: TypeHint,
    pub signature: Option<ExternalSignature>,
    pub namespace: Option<NamespaceRef>,
    pub intrinsic: Option<Intrinsic>,
}
impl ExternalSymbol {
    pub fn intrinsic(name: impl Into<String>, kind: Intrinsic) -> Self {
        Self {
            intrinsic: Some(kind),
            ..Self::dynamic(name)
        }
    }
    pub fn dynamic(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ty: TypeHint::Any,
            signature: None,
            namespace: None,
            intrinsic: None,
        }
    }
    pub fn value(name: impl Into<String>, ty: TypeHint) -> Self {
        Self {
            name: name.into(),
            ty,
            signature: None,
            namespace: None,
            intrinsic: None,
        }
    }
    pub fn function(name: impl Into<String>, signature: ExternalSignature) -> Self {
        Self {
            name: name.into(),
            ty: TypeHint::Any,
            signature: Some(signature),
            namespace: None,
            intrinsic: None,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError(pub String);
impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ResolveError {}
pub trait ModuleResolver {
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
}
impl MemoryResolver {
    pub fn insert(&mut self, module: ModuleInterface) {
        self.modules.insert(module.canonical_path.clone(), module);
    }
}
impl ModuleResolver for MemoryResolver {
    fn resolve(&mut self, r: &ImportRequest) -> Result<ResolvedImport, ResolveError> {
        let full = r.path.join("::");
        let namespace = self.modules.get(&full);
        let symbol = r.path.split_last().and_then(|(last, prefix)| {
            let module = self.modules.get(&prefix.join("::"))?;
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
                        ty: TypeHint::Table,
                        signature: None,
                        intrinsic: None,
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
        .ok_or_else(|| ResolveError(format!("module or public symbol {full} was not found")))
    }
}
