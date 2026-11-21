//! Versioned, injectable module interfaces. No IO or path policy lives here.
pub use aside_vm::bytecode::Const;
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
    StringLiteral(String),
    Record(Vec<(String, TypeHint)>),
    Enum {
        name: String,
        cases: Arc<Vec<TypeHint>>,
    },
}
impl std::fmt::Display for TypeHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Any => f.write_str("any"),
            Self::Closure => f.write_str("closure"),
            Self::Null => f.write_str("null"),
            Self::Bool => f.write_str("bool"),
            Self::Int => f.write_str("int"),
            Self::Float => f.write_str("float"),
            Self::String => f.write_str("string"),
            Self::Array => f.write_str("array"),
            Self::Table => f.write_str("table"),
            Self::Future => f.write_str("Future"),
            Self::Generator => f.write_str("generator"),
            Self::Named(name) => f.write_str(name),
            Self::StringLiteral(text) => write!(f, "{text:?}"),
            Self::Enum { name, .. } => f.write_str(name),
            Self::Record(fields) => {
                f.write_str("{")?;
                for (i, (name, ty)) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{name}: {ty}")?;
                }
                f.write_str("}")
            }
            Self::Tuple(types) => {
                f.write_str("(")?;
                for (i, ty) in types.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{ty}")?;
                }
                if types.len() == 1 {
                    f.write_str(",")?;
                }
                f.write_str(")")
            }
            Self::Union(types) => {
                for (i, ty) in types.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" | ")?;
                    }
                    if matches!(ty, Self::Callable(..)) {
                        write!(f, "({ty})")?;
                    } else {
                        write!(f, "{ty}")?;
                    }
                }
                Ok(())
            }
            Self::Callable(parameters, result) => {
                f.write_str("fn(")?;
                for (i, ty) in parameters.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{ty}")?;
                }
                f.write_str(") -> ")?;
                if matches!(result.as_ref(), Self::Union(_)) {
                    write!(f, "({result})")
                } else {
                    write!(f, "{result}")
                }
            }
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEffect {
    NoWait,
    MayWait,
    /// Starts concurrent work and requires a task context that allows waiting.
    StartsTask,
    Unknown,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalParameter {
    pub name: String,
    pub ty: TypeHint,
    pub mutable: bool,
    pub default: bool,
    pub rest: bool,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalSignature {
    pub parameters: Vec<ExternalParameter>,
    pub result: TypeHint,
    /// Refine a call result only when the referenced argument is a known source
    /// constant. Dynamic values and spreads retain `result` unchanged.
    pub result_cases: Vec<ExternalResultCase>,
    /// Refine the result from a supplied argument when its type is known.
    pub result_parameter: Option<ExternalResultParameter>,
    pub effect: CallEffect,
    pub init_safe: Option<bool>,
    pub derived_safe: Option<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalResultParameter {
    Value(String),
    Callback(String),
}
/// Source typing metadata; this does not change the native calling ABI.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalResultCase {
    pub parameter: String,
    pub equals: Const,
    pub result: TypeHint,
    /// Derived by the host declaration from the actual parameter default.
    pub when_omitted: bool,
}

/// Editor context for callbacks that consume the elements of a provider callback.
#[derive(Debug, Clone, Copy)]
pub struct SequenceCallbackContext {
    pub provider: &'static str,
    pub callbacks: &'static [SequenceCallback],
}

/// Describe the first argument of an inline callback using the provider's elements.
#[derive(Debug, Clone, Copy)]
pub struct SequenceCallback {
    pub parameter: &'static str,
    pub argument: SequenceCallbackArgument,
}

#[derive(Debug, Clone, Copy)]
pub enum SequenceCallbackArgument {
    Element,
    Record {
        element: &'static str,
        index: &'static str,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intrinsic {
    Bind,
    ValueOf,
    Derive,
    /// Declare the stable identity of an ordinary resumable function.
    ResumeFlow,
    /// Name the following complete waiting-operation or ordinary flow call.
    ResumePoint,
    /// Declare a persistent State contract for one static module class.
    ResumeState,
    /// Declare the current bootstrap and its persistent State input.
    ResumeRoot,
    /// A compile-time namespace and a tagged value contract.
    EnumType,
    /// A scalar tag or a known payload constructor within an enum namespace.
    EnumCase,
}
#[derive(Debug, Clone)]
pub struct NamespaceRef {
    pub module: String,
    pub version: String,
}
/// Canonical declaration identity supplied by a module resolver or the single
/// trusted native registry. A namespace references the module with export=None.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
/// A host-supplied scalar known before bytecode generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileConstant {
    Null,
    Bool(bool),
    Int(i64),
    FloatBits(u64),
    Utf8(String),
}

#[derive(Debug, Clone)]
pub struct ExternalSymbol {
    /// Immutable scalar supplied by the host at compilation. Reads emit its
    /// value directly and constant conditions can discard unreachable branches.
    pub constant: Option<Arc<CompileConstant>>,
    /// Bound source information shared through module interfaces. This never
    /// replaces the declared native signature or changes runtime admission.
    pub source_type: Option<Arc<super::SourceType>>,
    pub sequence_callbacks: Option<SequenceCallbackContext>,
    pub name: String,
    pub reference: Option<Arc<SymbolReference>>,
    pub resolved_from: Option<Arc<ResolutionSource>>,
    pub ty: TypeHint,
    pub signature: Option<ExternalSignature>,
    /// Source class shape. The class value exposes static members; constructed
    /// values use the canonical named instance type and its instance members.
    pub class: Option<Arc<ExternalClass>>,
    /// A type declaration supplies annotations and has no source value. Ordinary
    /// objects with a named type keep this false and retain normal value links.
    pub type_only: bool,
    pub namespace: Option<NamespaceRef>,
    pub intrinsic: Option<Intrinsic>,
    /// A discarded call result is a source error; explicit wildcard bindings
    /// and lexical children collection consume the result.
    pub must_use_result: bool,
}

/// A registered named type's property or bound method. `symbol.ty` is the read
/// type; `write_type: None` makes the member read-only. Method signatures exclude
/// the already bound receiver and preserve real parameter names and defaults.
#[derive(Debug, Clone)]
pub struct ExternalMember {
    pub symbol: ExternalSymbol,
    pub write_type: Option<TypeHint>,
}
/// A source class's declaration contracts, shared through import aliases.
#[derive(Debug, Clone)]
pub struct ExternalClass {
    pub bases: Vec<String>,
    pub members: Vec<ExternalMember>,
    pub static_members: Vec<ExternalMember>,
}
impl ExternalSymbol {
    /// Declare a typed compile-time value with ordinary import identity.
    pub fn constant(name: impl Into<String>, value: CompileConstant) -> Self {
        use CompileConstant as Const;
        let ty = match &value {
            Const::Null => TypeHint::Null,
            Const::Bool(_) => TypeHint::Bool,
            Const::Int(_) => TypeHint::Int,
            Const::FloatBits(_) => TypeHint::Float,
            Const::Utf8(_) => TypeHint::String,
        };
        Self {
            constant: Some(Arc::new(value)),
            ..Self::value(name, ty)
        }
    }
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
            constant: None,
            source_type: None,
            sequence_callbacks: None,
            name: name.into(),
            reference: None,
            resolved_from: None,
            ty: TypeHint::Any,
            signature: None,
            class: None,
            type_only: false,
            namespace: None,
            intrinsic: None,
            must_use_result: false,
        }
    }
    pub fn value(name: impl Into<String>, ty: TypeHint) -> Self {
        Self {
            constant: None,
            source_type: None,
            sequence_callbacks: None,
            name: name.into(),
            reference: None,
            resolved_from: None,
            ty,
            signature: None,
            class: None,
            type_only: false,
            namespace: None,
            intrinsic: None,
            must_use_result: false,
        }
    }
    pub fn function(name: impl Into<String>, signature: ExternalSignature) -> Self {
        Self {
            constant: None,
            source_type: None,
            sequence_callbacks: None,
            name: name.into(),
            reference: None,
            resolved_from: None,
            ty: TypeHint::Any,
            signature: Some(signature),
            class: None,
            type_only: false,
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
    /// A declared source enum's structural value contract.
    fn enum_type(&self, _canonical_name: &str) -> Option<TypeHint> {
        None
    }
    /// Whether this type has a complete, fixed member contract. A failed
    /// member lookup is a source error only when this returns true.
    fn has_closed_members(&self, _type_name: &str) -> bool {
        false
    }
    /// Resolve a real host type member without synthesizing a dynamic fallback.
    /// Source-only resolvers keep their existing module behavior by default.
    fn resolve_type_member(&mut self, _type_name: &str, _member: &str) -> Option<ExternalMember> {
        None
    }
    /// Return a trusted nominal relationship, or `None` when either type is not
    /// registered here and the existing runtime type check remains authoritative.
    fn is_named_subtype(&self, _actual: &str, _expected: &str) -> Option<bool> {
        None
    }
    /// The real callable contract of a named value, without inventing a member.
    fn resolve_type_call(&mut self, _name: &str) -> Option<ExternalSymbol> {
        None
    }
    /// The completed value produced by awaiting this registered named value.
    fn resolve_type_await(&mut self, _name: &str) -> Option<TypeHint> {
        None
    }
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
/// Resolve the longest matching module alias and preserve the remaining path.
pub fn canonical_module_path<'a>(
    path: &'a [String],
    aliases: &BTreeMap<String, String>,
) -> std::borrow::Cow<'a, [String]> {
    if aliases.is_empty() {
        return std::borrow::Cow::Borrowed(path);
    }
    let full = path.join("::");
    let mut end = full.len();
    loop {
        if let Some(canonical) = aliases
            .get(&full[..end])
            .filter(|canonical| *canonical != &full[..end])
        {
            let prefix = full[..end].split("::").count();
            let normalized = canonical
                .split("::")
                .map(str::to_owned)
                .chain(path[prefix..].iter().cloned())
                .collect();
            return std::borrow::Cow::Owned(normalized);
        }
        let Some(prefix) = full[..end].rfind("::") else {
            break;
        };
        end = prefix;
    }
    std::borrow::Cow::Borrowed(path)
}

/// Apply module aliases to an import while retaining its origin and import kind.
pub fn canonical_import<'a>(
    request: &'a ImportRequest,
    aliases: &BTreeMap<String, String>,
) -> std::borrow::Cow<'a, ImportRequest> {
    match canonical_module_path(&request.path, aliases) {
        std::borrow::Cow::Borrowed(_) => std::borrow::Cow::Borrowed(request),
        std::borrow::Cow::Owned(path) => std::borrow::Cow::Owned(ImportRequest {
            from: request.from.clone(),
            kind: request.kind,
            path,
        }),
    }
}
impl ModuleResolver for MemoryResolver {
    fn enum_type(&self, canonical_name: &str) -> Option<TypeHint> {
        let (module, name) = canonical_name.rsplit_once("::")?;
        let symbol = self
            .modules
            .get(module)?
            .exports
            .iter()
            .find(|symbol| symbol.name == name)?;
        matches!(symbol.ty, TypeHint::Enum { .. }).then(|| symbol.ty.clone())
    }
    fn canonical_request<'a>(
        &self,
        request: &'a ImportRequest,
    ) -> std::borrow::Cow<'a, ImportRequest> {
        canonical_import(request, &self.aliases)
    }
    fn resolve(&mut self, r: &ImportRequest) -> Result<ResolvedImport, ResolveError> {
        self.resolve_import(r)
    }
}
impl MemoryResolver {
    /// Resolve against immutable module registrations using the same import and
    /// alias rules as `ModuleResolver`, without copying the registered interfaces.
    pub fn resolve_import(&self, r: &ImportRequest) -> Result<ResolvedImport, ResolveError> {
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
                        constant: None,
                        source_type: None,
                        sequence_callbacks: None,
                        name: r.path.last().cloned().unwrap_or_default(),
                        reference: None,
                        resolved_from: None,
                        ty: TypeHint::Table,
                        signature: None,
                        class: None,
                        type_only: false,
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
