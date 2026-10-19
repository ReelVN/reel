//! Real source/compiler linkage using the same independent ABI v1 descriptions
//! as runtime admission. These functions compile; they do not execute the VM.
use super::{AdapterError, registry};
use aside_compiler::{
    AnalysisOptions, analyze_module,
    hir::AnalyzedModule,
    lowering,
    semantic::{ExternalSymbol, MemoryResolver, ModuleInterface, NamespaceRef},
};
use aside_vm::bytecode as bc;
pub use bc::FunctionId as StoryFunction;

pub struct CompiledStory {
    pub program: bc::VerifiedProgram,
    /// Run the real emitted module initializer before the selected story entry.
    pub initializer: bc::FunctionId,
    pub entry: bc::FunctionId,
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
    Entry,
    ProjectAnalysis(aside_compiler::semantic::ProjectAnalysis),
    ProjectLowering(lowering::ProjectError),
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
}

pub struct ProjectSourceCompilation {
    pub modules: Vec<AnalyzedModule>,
    pub compilation: lowering::ProjectCompilation,
}

/// The author CLI and runtime share this resolver and trusted native registry.
/// `content_version` is supplied by the asset/module resolver, not a save ID.
fn environment() -> (
    Vec<registry::NativeContract>,
    MemoryResolver,
    Vec<ExternalSymbol>,
) {
    let contracts = registry::contracts();
    let references = contracts
        .iter()
        .map(registry::NativeContract::reference)
        .collect::<Vec<_>>();
    let exports = contracts
        .iter()
        .map(|contract| {
            let mut external = contract.external();
            if contract.symbol == "reel::derive" {
                external.intrinsic = Some(aside_compiler::semantic::Intrinsic::Derive);
            }
            external
        })
        .collect::<Vec<_>>();
    let mut resolver = MemoryResolver::default();
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
    let mut stage = ExternalSymbol::dynamic("stage");
    prelude.extend(aside_std::prelude());
    stage.namespace = Some(NamespaceRef {
        module: "reel::stage".into(),
        version: "1".into(),
    });
    prelude.push(stage);
    prelude.push(ExternalSymbol::intrinsic(
        "bind",
        aside_compiler::semantic::Intrinsic::Bind,
    ));
    let mut game = ExternalSymbol::dynamic("game");
    game.namespace = Some(NamespaceRef {
        module: "reel::game".into(),
        version: "1".into(),
    });
    prelude.push(game);
    let mut audio = ExternalSymbol::dynamic("audio");
    audio.namespace = Some(NamespaceRef {
        module: "reel::audio".into(),
        version: "1".into(),
    });
    prelude.push(audio);
    let mut localization = ExternalSymbol::dynamic("localization");
    localization.namespace = Some(NamespaceRef {
        module: "reel::localization".into(),
        version: "1".into(),
    });
    prelude.push(localization);
    for name in ["saves", "persistent", "window", "mask"] {
        let module = format!("reel::{name}");
        let mut namespace = ExternalSymbol::dynamic(name);
        namespace.namespace = Some(NamespaceRef {
            module: module.clone(),
            version: "1".into(),
        });
        prelude.push(namespace);
        resolver.insert(ModuleInterface {
            canonical_path: module.clone(),
            version: "1".into(),
            exports: exports
                .iter()
                .zip(&references)
                .filter(|(_, r)| r.module == module)
                .map(|(e, _)| e.clone())
                .collect(),
        });
    }
    resolver.insert(ModuleInterface {
        canonical_path: "reel::localization".into(),
        version: "1".into(),
        exports: exports
            .iter()
            .zip(&references)
            .filter(|(_, r)| r.module == "reel::localization")
            .map(|(e, _)| e.clone())
            .collect(),
    });
    resolver.insert(ModuleInterface {
        canonical_path: "reel::audio".into(),
        version: "1".into(),
        exports: exports
            .iter()
            .zip(&references)
            .filter(|(_, r)| r.module == "reel::audio" && r.export.as_deref() != Some("await"))
            .map(|(e, _)| e.clone())
            .collect(),
    });

    for path in ["reel", "reel::prelude"] {
        resolver.insert(ModuleInterface {
            canonical_path: path.into(),
            version: "1".into(),
            exports: prelude.clone(),
        });
    }
    resolver.insert(ModuleInterface {
        canonical_path: "reel::stage".into(),
        version: "1".into(),
        exports: exports
            .iter()
            .zip(&references)
            .filter(|(_, reference)| reference.module == "reel::stage")
            .map(|(export, _)| export.clone())
            .collect(),
    });
    resolver.insert(ModuleInterface {
        canonical_path: "reel::game".into(),
        version: "1".into(),
        exports: exports
            .iter()
            .zip(&references)
            .filter(|(_, r)| r.module == "reel::game")
            .map(|(e, _)| e.clone())
            .collect(),
    });
    (contracts, resolver, prelude)
}

pub fn compile_module(
    source: &str,
    module_path: &str,
    source_path: &str,
    content_version: u64,
) -> Result<SourceCompilation, SourceError> {
    let (contracts, mut resolver, prelude) = environment();
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
    let compiled = aside_host::lower(
        &module,
        &contracts,
        &registry::capabilities(),
        lowering::Options {
            content_version,
            ..Default::default()
        },
    )
    .map_err(|error| match error {
        aside_host::BuildError::Format(error) => SourceError::Format(error),
        aside_host::BuildError::Lowering(error) => SourceError::Lowering(error),
    })?;
    Ok(SourceCompilation {
        module,
        compilation: compiled,
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
    let entry = compiled.metadata.functions[function_index].output;
    let story = CompiledStory {
        program: compiled.program,
        initializer: compiled.entry,
        entry,
    };
    story.validate()?;
    Ok(story)
}

pub fn compile_project(
    inputs: &[aside_compiler::semantic::SourceModule],
    aliases: &std::collections::BTreeMap<String, String>,
    content_version: u64,
) -> Result<ProjectSourceCompilation, SourceError> {
    let (contracts, mut resolver, prelude) = environment();
    resolver.aliases = aliases.clone();
    let options = AnalysisOptions {
        version: content_version.to_string(),
        hosts: prelude,
        ..Default::default()
    };
    let mut analysis = aside_compiler::semantic::analyze_project(inputs, &options, &mut resolver)
        .map_err(SourceError::Source)?;
    if !analysis.is_ok() {
        return Err(SourceError::ProjectAnalysis(analysis));
    }
    let plan = std::mem::take(&mut analysis.plan);
    let modules = analysis
        .into_modules()
        .map_err(SourceError::ProjectAnalysis)?;
    let compilation = aside_host::lower_project(
        &modules,
        &plan,
        &contracts,
        &registry::capabilities(),
        lowering::Options {
            content_version,
            ..Default::default()
        },
    )
    .map_err(SourceError::ProjectLowering)?;
    Ok(ProjectSourceCompilation {
        modules,
        compilation,
    })
}

impl From<SourceError> for AdapterError {
    fn from(error: SourceError) -> Self {
        Self::Source(error)
    }
}
