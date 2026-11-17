//! Source compilation and editor metadata share ABI v1 descriptions with runtime admission.
use super::{AdapterError, registry};
use aside_compiler::{
    AnalysisOptions, analyze_module,
    hir::AnalyzedModule,
    lowering,
    semantic::{ExternalSymbol, MemoryResolver, ModuleInterface, NamespaceRef},
};
use aside_host::source::{Api, Declaration};
use aside_vm::bytecode as bc;
pub use bc::FunctionId as StoryFunction;
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

mod effects;
mod translations;

/// Offer the literal values accepted by a native constructor's property validator.
pub fn literal_argument_choices(module: &str, export: &str, parameter: &str) -> Vec<&'static str> {
    use super::stage::{property_values, scroll};
    match (module, export, parameter) {
        ("reel", "image" | "video", "fit") => property_values::FIT_CHOICES
            .into_iter()
            .map(|(name, _)| name)
            .collect(),
        ("reel", "image" | "video", "sampling") => property_values::SAMPLING_CHOICES
            .into_iter()
            .map(|(name, _)| name)
            .collect(),
        ("reel", "scroll", "overscroll") => scroll::OVERSCROLL_CHOICES
            .into_iter()
            .map(|(name, _)| name)
            .collect(),
        _ => Vec::new(),
    }
}

pub use effects::{
    CompiledEffect, EffectCompileLimits, EffectDefinition, EffectDefinitionId, EffectDiagnostic,
    EffectFunction, EffectLoadLimits, EffectLocation, EffectOrigin, EffectProductError,
    EffectProducts, EffectSource, collect_effects, compile_effect, decode_definition,
    encode_definition, encode_mask_template, resolve_effect_function,
};
pub use translations::{
    TranslationAnalysis, TranslationDiagnostic, TranslationHole, TranslationSite,
    TranslationVariant, analyze_translations, extract_translations,
};

/// Locale catalog text emitted by the same analyzed source compilation. These
/// are build products; release packaging stores each locale as a seekable asset.
#[derive(Default, Debug)]
pub struct CatalogProducts {
    pub catalogs: BTreeMap<String, String>,
    pub messages: Vec<TranslationMessage>,
}

/// Generated lookup identity with the author's original diagnostic location.
#[derive(Debug)]
pub struct TranslationMessage {
    pub key: String,
    pub source: String,
    pub module: String,
    pub span: aside_compiler::TextRange,
}

static SOURCE_API: OnceLock<Api> = OnceLock::new();

/// Trusted native ABI declarations, shared by compilation and VM admission.
pub fn contracts() -> &'static [aside_host::NativeContract] {
    static CONTRACTS: OnceLock<Vec<aside_host::NativeContract>> = OnceLock::new();
    CONTRACTS.get_or_init(registry::contracts)
}

/// Precise source types and English declarations shared by compilation and editors.
/// The immutable registry is constructed once for the process.
pub fn api() -> &'static Api {
    SOURCE_API.get_or_init(build_api)
}

/// Trusted nominal registrations derived from the same source type declarations.
/// ASB-only playback retains the registration projection and releases the
/// temporary source schema. Compilation and editors reuse their existing cache.
pub fn types() -> &'static [bc::HostTypeSupport] {
    static TYPES: OnceLock<Vec<bc::HostTypeSupport>> = OnceLock::new();
    TYPES.get_or_init(|| match SOURCE_API.get() {
        Some(api) => api.registrations(),
        None => build_api().registrations(),
    })
}

/// Heap-local credentials are shared by the session's real host constructors.
pub(super) struct HostTypes(std::collections::BTreeMap<&'static str, aside_vm::HostTypeId>);
impl HostTypes {
    pub(super) fn new(heap: &aside_vm::Heap) -> Result<Self, aside_vm::HeapError> {
        Ok(Self(
            types()
                .iter()
                .map(|ty| Ok((ty.name.as_str(), heap.host_type(&ty.name)?)))
                .collect::<Result<_, aside_vm::HeapError>>()?,
        ))
    }
    pub(super) fn mark(
        &self,
        heap: &mut aside_vm::Heap,
        value: aside_vm::Value,
        name: &str,
    ) -> Result<(), aside_vm::HeapError> {
        heap.set_host_type(value, self.0[name])
    }
}

/// Read the current registered signatures and English source documentation.
/// Editor clients use the resolved canonical symbol to select a hover entry.
pub fn documentation() -> Vec<aside_host::NativeDocumentation> {
    let declared = aside_std::documentation()
        .into_iter()
        .chain(
            super::stage::bindings()
                .into_iter()
                .map(aside_host::NativeBinding::into_documentation),
        )
        .collect::<Vec<_>>();
    let mut signatures = declared
        .iter()
        .map(|entry| (entry.contract.symbol.clone(), entry.signature.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut markdown: std::collections::BTreeMap<_, _> = declared
        .into_iter()
        .map(|entry| (entry.contract.symbol, entry.markdown))
        .collect();
    markdown.insert("reel::when".into(), super::conditions::DOCUMENTATION);
    contracts()
        .iter()
        .filter(|contract| {
            !matches!(
                contract.symbol.as_str(),
                "reel::when::evaluate"
                    | "reel::page_stack::produce"
                    | "reel::page_stack::commit_selection"
                    | "reel::tab_bar::observe_label"
                    | super::stage::text_input::filter::SYMBOL
            )
        })
        .cloned()
        .map(|contract| {
            let documentation = markdown
                .remove(&contract.symbol)
                .filter(|text| !text.is_empty())
                .or_else(|| super::narrative::documentation(&contract.symbol))
                .or_else(|| super::activity::documentation(&contract.symbol))
                .or_else(|| super::scopes::documentation(&contract.symbol))
                .or_else(|| super::checkpoint::documentation(&contract.symbol))
                .or_else(|| super::results::documentation(&contract.symbol))
                .or_else(|| super::observable::documentation(&contract.symbol))
                .or_else(|| super::lists::documentation(&contract.symbol))
                .or_else(|| super::pages::documentation(&contract.symbol))
                .or_else(|| super::views::documentation(&contract.symbol))
                .or_else(|| super::layered_image::documentation(&contract.symbol))
                .or_else(|| super::localization::documentation(&contract.symbol))
                .or_else(|| super::rich_text::documentation(&contract.symbol))
                .or_else(|| super::particles::documentation(&contract.symbol))
                .or_else(|| super::stage::documentation(&contract.symbol))
                .or_else(|| super::stage::scrollbar::documentation(&contract.symbol))
                .or_else(|| super::stage::scroll::documentation(&contract.symbol))
                .or_else(|| super::stage::text_input::documentation(&contract.symbol))
                .or_else(|| super::stage::configure::documentation(&contract.symbol))
                .or_else(|| super::game::documentation(&contract.symbol))
                .or_else(|| super::transitions::documentation(&contract.symbol))
                .or_else(|| super::results::geometry::documentation(&contract.symbol))
                .or_else(|| super::masks::documentation(&contract.symbol))
                .or_else(|| super::effects::documentation(&contract.symbol))
                .or_else(|| super::gradients::documentation(&contract.symbol))
                .or_else(|| super::stage::interaction_appearance::documentation(&contract.symbol))
                .or_else(|| super::stage::image_skin::documentation(&contract.symbol))
                .or_else(|| super::media::documentation(&contract.symbol))
                .or_else(|| super::data::documentation(&contract.symbol))
                .or_else(|| super::saves::documentation(&contract.symbol))
                .or_else(|| super::window::documentation(&contract.symbol))
                .or_else(|| super::input_hints::documentation(&contract.symbol))
                .or_else(|| super::capture::documentation(&contract.symbol))
                .or_else(|| super::assets::documentation(&contract.symbol))
                .or_else(|| super::stage::input::documentation(&contract.symbol))
                .unwrap_or("");
            aside_host::NativeDocumentation {
                signature: signatures
                    .remove(&contract.symbol)
                    .unwrap_or_else(|| contract.external().signature.unwrap()),
                contract,
                markdown: documentation,
            }
        })
        .collect()
}

/// Query common node properties without constructing a node or a VM session.
pub fn node_property_documentation()
-> impl ExactSizeIterator<Item = aside_host::PropertyDocumentation> {
    use aside_host::PropertySchema;
    super::stage::NodeProperty::PROPERTIES
        .iter()
        .enumerate()
        .map(|(index, property)| aside_host::PropertyDocumentation {
            property,
            markdown: super::stage::NodeProperty::documentation(index),
        })
}

pub struct CompiledStory {
    pub program: bc::VerifiedProgram,
    /// Stable continuation metadata verified against this compilation.
    pub resume: aside_vm::resume::VerifiedResumeTable,
    /// Run the real emitted module initializer before the selected story entry.
    pub initializer: bc::FunctionId,
    pub entry: bc::FunctionId,
    /// Source compilation products, consumed by `SessionVm::load_story`.
    /// Bytecode assets use an empty product and their indexed asset loader.
    pub catalogs: Arc<CatalogProducts>,
    /// Validated effect definitions indexed by final linked FunctionId.
    pub effects: Arc<EffectProducts>,
}

impl CompiledStory {
    /// Validate host entry metadata in addition to B01 verification. Native
    /// admission remains exclusively the responsibility of SessionVm::load.
    pub fn validate(&self) -> Result<(), SourceError> {
        let p = self.program.package();
        validate_entry(&self.program, self.entry)?;
        let initializer = p
            .functions
            .get(self.initializer.index())
            .ok_or(SourceError::Entry)?;
        let init_signature = &p.signatures[initializer.signature.index()];
        if initializer.kind != bc::FunctionKind::Plain
            || initializer.context != bc::Context::Init
            || !initializer.captures.is_empty()
            || !init_signature.parameters.is_empty()
            || init_signature.rest.is_some()
            || p.modules[initializer.module.index()].initializer != Some(self.initializer)
        {
            return Err(SourceError::Entry);
        }
        Ok(())
    }
}

pub(super) fn validate_entry(
    program: &bc::VerifiedProgram,
    entry: bc::FunctionId,
) -> Result<(), SourceError> {
    let p = program.package();
    let function = p.functions.get(entry.index()).ok_or(SourceError::Entry)?;
    let signature = &p.signatures[function.signature.index()];
    if !function.captures.is_empty()
        || function.kind != bc::FunctionKind::Plain
        || function.context != bc::Context::Ordinary
        || signature
            .parameters
            .iter()
            .any(|parameter| parameter.default.is_none())
    {
        return Err(SourceError::Entry);
    }
    Ok(())
}

#[derive(Debug)]
pub enum SourceError {
    Source(aside_compiler::SourceTooLarge),
    Analysis(aside_compiler::semantic::AnalysisFailure),
    Lowering(Box<lowering::Error>),
    Format(bc::Error),
    Resume(aside_vm::resume::ResumeError),
    Entry,
    ProjectAnalysis(aside_compiler::semantic::ProjectAnalysis),
    ProjectLowering(lowering::ProjectError),
    Translation(TranslationDiagnostic),
    Effect(EffectDiagnostic),
    EffectProduct(EffectProductError),
}
impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for SourceError {}

/// One source analysis and its B01 lowering, shared with author tools.
pub struct SourceCompilation {
    pub module: AnalyzedModule,
    pub compilation: lowering::Compilation,
    pub catalogs: Arc<CatalogProducts>,
    pub effects: Arc<EffectProducts>,
}

/// Verified project products without retained source-analysis arenas.
pub struct ProjectSourceCompilation {
    pub compilation: lowering::ProjectCompilation,
    pub catalogs: Arc<CatalogProducts>,
    pub effects: Arc<EffectProducts>,
}

/// The author CLI and runtime share this resolver and trusted native registry.
/// `content_version` is supplied by the asset/module resolver, not a save ID.
fn build_api() -> Api {
    let contracts = contracts();
    let mut api = Api {
        functions: documentation()
            .into_iter()
            .map(|doc| {
                let mut declaration = Declaration::native(&doc.contract, doc.markdown);
                declaration.symbol.signature = Some(doc.signature);
                declaration
            })
            .collect(),
        ..Default::default()
    };
    aside_std::describe(&mut api);
    super::stage::source::describe(&mut api);
    super::dialogue::describe(&mut api);
    super::results::describe(&mut api);
    super::saves::describe(&mut api);
    super::game::describe(&mut api);
    super::scopes::describe(&mut api);
    super::checkpoint::describe(&mut api);
    super::activity::describe(&mut api);
    super::media::describe(&mut api);
    super::rich_text::describe(&mut api);
    super::narrative::describe(&mut api);
    super::masks::describe(&mut api);
    super::effects::describe(&mut api);
    super::gradients::describe(&mut api);
    super::stage::interaction_appearance::describe(&mut api);
    super::stage::image_skin::describe(&mut api);
    super::capture::describe(&mut api);
    super::assets::describe(&mut api);
    super::window::describe(&mut api);
    super::input_hints::describe(&mut api);
    super::particles::describe(&mut api);
    super::activity::describe_controls(&mut api);
    super::localization::describe(&mut api);
    super::transitions::describe(&mut api);
    super::stage::configure::describe(&mut api);
    super::lists::describe(&mut api);
    super::pages::describe(&mut api);
    super::stage::scrollbar::describe(&mut api);
    api.refine(
        "reel::derive",
        aside_host::source::named("Binding"),
        &[("compute", aside_compiler::semantic::TypeHint::Closure)],
    );
    let owner = aside_host::source::nullable(aside_compiler::semantic::TypeHint::Union(vec![
        aside_host::source::named("reel::types::Node"),
        aside_host::source::named("reel::types::Scope"),
    ]));
    for declaration in &mut api.functions {
        let symbol = aside_host::source::canonical(&declaration.symbol);
        if !matches!(
            symbol.as_str(),
            "reel::scope" | "reel::reading" | "reel::stage::move_pointer"
        ) && let Some(signature) = &mut declaration.symbol.signature
        {
            for parameter in &mut signature.parameters {
                if parameter.name == "owner" {
                    parameter.ty = owner.clone();
                }
            }
        }
    }
    let references = contracts
        .iter()
        .map(registry::NativeContract::reference)
        .collect::<Vec<_>>();
    let exports = contracts
        .iter()
        .map(|contract| {
            let mut external = api
                .functions
                .iter()
                .find(|declaration| {
                    declaration.symbol.reference.as_deref() == Some(&contract.reference())
                })
                .map(|declaration| declaration.symbol.clone())
                .unwrap_or_else(|| contract.external());
            external.must_use_result = registry::is_node_constructor(&contract.symbol);
            if contract.symbol == "reel::derive" {
                external.intrinsic = Some(aside_compiler::semantic::Intrinsic::Derive);
            }
            external
        })
        .collect::<Vec<_>>();
    let mut resolver = MemoryResolver::default();
    let target = Declaration::constant(
        "reel::build",
        "1",
        "target",
        aside_compiler::semantic::CompileConstant::Utf8(std::env::consts::OS.into()),
        "The build target selected by --resource-target, such as windows, linux, macos or vita. Use ordinary if conditions to select platform effects or settings at compilation; unselected branches produce no executable body. Resource overlays use the same target. window.info().platform reports the running player instead.",
    );
    resolver.insert(ModuleInterface {
        canonical_path: "reel::build".into(),
        version: "1".into(),
        exports: vec![target.symbol.clone()],
    });
    api.constants.push(target);
    for module in aside_std::modules() {
        resolver.insert(module);
    }
    let mut prelude = exports
        .iter()
        .zip(&references)
        .filter(|(_, reference)| reference.module == "reel")
        .map(|(export, _)| export.clone())
        .collect::<Vec<_>>();
    prelude
        .iter_mut()
        .find(|e| e.name == "video")
        .unwrap()
        .namespace = Some(NamespaceRef {
        module: "reel::video".into(),
        version: "1".into(),
    });
    resolver.insert(ModuleInterface {
        canonical_path: "reel::video".into(),
        version: "1".into(),
        exports: exports
            .iter()
            .zip(&references)
            .filter(|(_, r)| r.module == "reel::video")
            .map(|(e, _)| e.clone())
            .collect(),
    });
    prelude.extend(aside_std::prelude());
    prelude.push(ExternalSymbol::intrinsic(
        "bind",
        aside_compiler::semantic::Intrinsic::Bind,
    ));
    {
        use aside_compiler::semantic::{
            CallEffect, ExternalParameter, ExternalSignature, Intrinsic, TypeHint,
        };
        let mut value_of = ExternalSymbol::function(
            "value_of",
            ExternalSignature {
                parameters: vec![ExternalParameter {
                    name: "value".into(),
                    ty: TypeHint::Any,
                    mutable: false,
                    default: false,
                    rest: false,
                }],
                result: TypeHint::Any,
                result_cases: Vec::new(),
                result_parameter: None,
                effect: CallEffect::NoWait,
                init_safe: Some(true),
                derived_safe: Some(true),
            },
        );
        value_of.intrinsic = Some(Intrinsic::ValueOf);
        prelude.push(value_of);
    }
    let build = Declaration::namespace(
        "reel::build",
        "1",
        "Values fixed when the game is compiled.",
    );
    prelude.push(build.symbol.clone());
    api.namespaces.push(build);
    for (module, documentation) in [
        (
            "reel::assets",
            "Read immutable data through the same packaged or loose resource namespace as images and media.",
        ),
        (
            "reel::stage",
            "Mount, inspect and capture the common stage, and query or request pointer movement.",
        ),
        (
            "reel::game",
            "Configure the game session, start a fresh story, and access dialogue, choices, reading state and history.",
        ),
        (
            "reel::audio",
            "Play and control audio activities, queue sources, and await playback positions.",
        ),
        (
            "reel::localization",
            "Load translation catalogs and select or read the current observable locale.",
        ),
        (
            "reel::saves",
            "Save, load, copy, list and delete story slots, and retrieve their captured thumbnails.",
        ),
        (
            "reel::persistent",
            "Read, write and delete persistent data shared across story sessions.",
        ),
        (
            "reel::window",
            "Read portable window information and request supported window configuration changes.",
        ),
        (
            "reel::mask",
            "Build immutable scalar mask expressions from image alpha, grayscale and arithmetic.",
        ),
    ] {
        let declaration = Declaration::namespace(module, "1", documentation);
        prelude.push(declaration.symbol.clone());
        api.namespaces.push(declaration);
        resolver.insert(ModuleInterface {
            canonical_path: module.into(),
            version: "1".into(),
            exports: exports
                .iter()
                .zip(&references)
                .filter(|(_, reference)| {
                    reference.module == module
                        && !(module == "reel::audio"
                            && reference.export.as_deref() == Some("await"))
                })
                .map(|(e, _)| e.clone())
                .collect(),
        });
    }

    prelude.extend(api.types.iter().map(aside_host::source::NamedType::symbol));
    for path in ["reel", "reel::prelude"] {
        resolver.insert(ModuleInterface {
            canonical_path: path.into(),
            version: "1".into(),
            exports: prelude.clone(),
        });
    }
    let mut type_modules: std::collections::BTreeMap<String, Vec<ExternalSymbol>> =
        Default::default();
    for ty in &api.types {
        let symbol = ty.symbol();
        type_modules
            .entry(symbol.reference.as_ref().unwrap().module.clone())
            .or_default()
            .push(symbol);
    }
    for (canonical_path, exports) in type_modules {
        if let Some(module) = resolver.modules.get_mut(&canonical_path) {
            for symbol in exports {
                if !module
                    .exports
                    .iter()
                    .any(|existing| existing.name == symbol.name)
                {
                    module.exports.push(symbol);
                }
            }
        } else {
            resolver.insert(ModuleInterface {
                canonical_path,
                version: "1".into(),
                exports,
            });
        }
    }
    for symbol in prelude.iter_mut().chain(
        resolver
            .modules
            .values_mut()
            .flat_map(|module| module.exports.iter_mut()),
    ) {
        if let Some(declaration) = api
            .functions
            .iter()
            .find(|declaration| declaration.symbol.reference == symbol.reference)
        {
            symbol.signature = declaration.symbol.signature.clone();
        }
    }
    api.globals = prelude;
    api.modules = resolver.modules.into_values().collect();
    api
}

struct SourceEnvironment(MemoryResolver);
impl aside_compiler::semantic::ModuleResolver for SourceEnvironment {
    fn has_closed_members(&self, ty: &str) -> bool {
        api().types.iter().any(|named| named.name == ty)
    }
    fn resolve_type_call(&mut self, ty: &str) -> Option<ExternalSymbol> {
        api()
            .types
            .iter()
            .find(|named| named.name == ty)?
            .call
            .as_ref()
            .map(|declaration| declaration.symbol.clone())
    }
    fn resolve_type_await(&mut self, ty: &str) -> Option<aside_compiler::semantic::TypeHint> {
        api()
            .types
            .iter()
            .find(|named| named.name == ty)?
            .await_result
            .clone()
    }
    fn resolve_type_member(
        &mut self,
        ty: &str,
        member: &str,
    ) -> Option<aside_compiler::semantic::ExternalMember> {
        api().member(ty, member)
    }
    fn is_named_subtype(&self, actual: &str, expected: &str) -> Option<bool> {
        api().is_subtype(actual, expected)
    }
    fn canonical_request<'a>(
        &self,
        request: &'a aside_compiler::semantic::ImportRequest,
    ) -> std::borrow::Cow<'a, aside_compiler::semantic::ImportRequest> {
        aside_compiler::semantic::ModuleResolver::canonical_request(&self.0, request)
    }
    fn resolve(
        &mut self,
        request: &aside_compiler::semantic::ImportRequest,
    ) -> Result<aside_compiler::semantic::ResolvedImport, aside_compiler::semantic::ResolveError>
    {
        aside_compiler::semantic::ModuleResolver::resolve(&mut self.0, request)
    }
}
fn environment(target: &str) -> (SourceEnvironment, Vec<ExternalSymbol>) {
    let mut resolver = MemoryResolver::default();
    for module in &api().modules {
        resolver.insert(module.clone());
    }
    resolver
        .modules
        .get_mut("reel::build")
        .expect("registered build module")
        .exports[0]
        .constant = Some(Arc::new(aside_compiler::semantic::CompileConstant::Utf8(
        target.into(),
    )));
    (SourceEnvironment(resolver), api().globals.clone())
}

pub fn compile_module(
    source: &str,
    module_path: &str,
    source_path: &str,
    content_version: u64,
) -> Result<SourceCompilation, SourceError> {
    let (mut resolver, prelude) = environment(std::env::consts::OS);
    let options = AnalysisOptions {
        module: module_path.into(),
        source_path: Some(source_path.into()),
        version: content_version.to_string(),
        hosts: prelude,
        ..Default::default()
    };
    let module = analyze_module(source, &options, &mut resolver)
        .map_err(SourceError::Source)?
        .into_module()
        .map_err(SourceError::Analysis)?;
    let (catalogs, expansions) =
        extract_translations(std::slice::from_ref(&module)).map_err(SourceError::Translation)?;
    let effects =
        effects::collect_registered(std::slice::from_ref(&module)).map_err(SourceError::Effect)?;
    let compiled = aside_host::lower(
        &module,
        contracts(),
        &api().functions,
        types(),
        &registry::capabilities(),
        aside_host::CompileOptions {
            lowering: lowering::Options {
                content_version,
                ..Default::default()
            },
            expansions: &expansions,
        },
    )
    .map_err(|error| match error {
        aside_host::BuildError::Format(error) => SourceError::Format(error),
        aside_host::BuildError::Lowering(error) => SourceError::Lowering(error),
    })?;
    let effects = effects::bind_single(effects, &compiled).map_err(SourceError::EffectProduct)?;
    Ok(SourceCompilation {
        module,
        compilation: compiled,
        catalogs: Arc::new(catalogs),
        effects: Arc::new(effects),
    })
}

pub fn compile_story(
    source: &str,
    module_path: &str,
    content_version: u64,
    entry_name: &str,
) -> Result<CompiledStory, SourceError> {
    let SourceCompilation {
        module,
        compilation: compiled,
        catalogs,
        effects,
    } = compile_module(source, module_path, module_path, content_version)?;
    // The compiler initializes module function globals at runtime; their
    // GlobalDef.linked_function is intentionally None. Select the real HIR
    // declaration/output mapping, not that optional prelinked-closure field.
    let root_scope = module.facts()[0].scope.ok_or(SourceError::Entry)?;
    let declared = module
        .symbols()
        .iter()
        .position(|symbol| {
            symbol.scope == root_scope
                && module.hir().name(symbol.name) == entry_name
                && symbol.kind == aside_compiler::hir::SymbolKind::Function
        })
        .map(|index| aside_compiler::hir::SymbolId(index as u32))
        .ok_or(SourceError::Entry)?;
    let function_index = module
        .functions()
        .iter()
        .position(|function| module.facts()[function.node.0 as usize].symbol == Some(declared))
        .ok_or(SourceError::Entry)?;
    let entry = compiled.metadata.functions[function_index]
        .output
        .ok_or(SourceError::Entry)?;
    let resume = compiled
        .program
        .verify_resume(
            compiled.resume,
            crate::aside::source::types(),
            Default::default(),
        )
        .map_err(SourceError::Resume)?;
    let story = CompiledStory {
        program: compiled.program,
        resume,
        initializer: compiled.entry,
        entry,
        catalogs,
        effects,
    };
    story.validate()?;
    Ok(story)
}

pub fn compile_project(
    inputs: &[aside_compiler::semantic::SourceModule],
    aliases: &std::collections::BTreeMap<String, String>,
    content_version: u64,
) -> Result<ProjectSourceCompilation, SourceError> {
    compile_project_with_progress(
        inputs,
        aliases,
        content_version,
        std::env::consts::OS,
        &mut |_| {},
    )
}

/// Run the same pipeline while reporting completed stages and modules.
pub fn compile_project_with_progress(
    inputs: &[aside_compiler::semantic::SourceModule],
    aliases: &std::collections::BTreeMap<String, String>,
    content_version: u64,
    target: &str,
    progress: &mut dyn FnMut(aside_compiler::Progress),
) -> Result<ProjectSourceCompilation, SourceError> {
    let (mut resolver, prelude) = environment(target);

    resolver.0.aliases = aliases.clone();
    let options = AnalysisOptions {
        version: content_version.to_string(),
        hosts: prelude,
        ..Default::default()
    };
    let mut analysis = aside_compiler::semantic::analyze_project_with_progress(
        inputs,
        &options,
        &mut resolver,
        progress,
    )
    .map_err(SourceError::Source)?;

    if !analysis.is_ok() {
        return Err(SourceError::ProjectAnalysis(analysis));
    }
    let plan = std::mem::take(&mut analysis.plan);
    let modules = analysis
        .into_modules()
        .map_err(SourceError::ProjectAnalysis)?;

    let (catalogs, expansions) =
        extract_translations(&modules).map_err(SourceError::Translation)?;
    let effects = effects::collect_registered(&modules).map_err(SourceError::Effect)?;
    let module_paths = modules
        .iter()
        .map(|module| module.canonical_path().to_owned())
        .collect::<Vec<_>>();

    let compilation = aside_host::lower_project_with_progress(
        modules,
        &plan,
        contracts(),
        &api().functions,
        types(),
        &registry::capabilities(),
        aside_host::CompileOptions {
            lowering: lowering::Options {
                content_version,
                ..Default::default()
            },
            expansions: &expansions,
        },
        progress,
    )
    .map_err(SourceError::ProjectLowering)?;

    let effects = effects::bind_project(effects, &module_paths, &compilation)
        .map_err(SourceError::EffectProduct)?;
    Ok(ProjectSourceCompilation {
        compilation,
        catalogs: Arc::new(catalogs),
        effects: Arc::new(effects),
    })
}

impl From<SourceError> for AdapterError {
    fn from(error: SourceError) -> Self {
        Self::Source(error)
    }
}
