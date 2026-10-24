use super::{Checked, Failure, error};
use aside_compiler::{DiagnosticCode, TextRange, lowering::ProjectModule};
use aside_vm::bytecode as bc;
use reel_package as rp;
use serde_json::{Value, json};

fn span(range: TextRange) -> rp::Span {
    rp::Span {
        start: range.start().into(),
        end: range.end().into(),
    }
}
fn text_range(span: rp::Span) -> TextRange {
    TextRange::new(span.start.into(), span.end.into())
}

pub(crate) fn module_metadata(
    compiled: &ProjectModule,
    program: &bc::VerifiedProgram,
) -> rp::ModuleMetadata {
    let m = &compiled.metadata;
    let program = program.package();
    let declarations = m
        .declarations
        .iter()
        .map(|declaration| rp::Declaration {
            name: program.symbols[declaration.name.index()].clone(),
            source_symbol_id: declaration.source_symbol.0,
            source_function_id: declaration.source_function.0,
            global_id: declaration.global.0,
            function_id: declaration.output.0,
        })
        .collect();
    let declaration_spans = m
        .declarations
        .iter()
        .map(|declaration| span(declaration.span))
        .collect();
    rp::ModuleMetadata {
        module_id: compiled.module.0,
        canonical_path: m.canonical_path.clone(),
        source_version: m.source_version.clone(),
        debug: Some(rp::ModuleDebug {
            source_blake3: rp::digest(program.sources[compiled.source.index()].text.as_bytes()),
            source_id: compiled.source.0,
            source_span: span(m.source_span),
            declaration_spans,
        }),
        symbol_count: m.symbol_count,
        initializer: rp::Initializer {
            module_id: compiled.module.0,
            function_id: compiled.initializer.0,
        },
        functions: m
            .functions
            .iter()
            .map(|f| rp::FunctionMetadata {
                output: f.output.0,
                declared_context: rp::DeclaredContext {
                    no_wait: f.declared_context.no_wait,
                    init: f.declared_context.init,
                    derived: f.declared_context.derived,
                },
                known_wait: f.known_wait,
                dynamic_calls: f.dynamic_calls,
            })
            .collect(),
        bindings: m
            .bindings
            .iter()
            .map(|b| rp::BindingIdentity {
                function: b.function.0,
                binding: b.binding.0,
                source: b.source.0,
            })
            .collect(),
        globals: m
            .globals
            .iter()
            .map(|g| rp::GlobalIdentity {
                global: g.global.0,
                source: g.source.0,
            })
            .collect(),
        declarations,
    }
}

pub struct Artifact {
    pub bytes: Vec<u8>,
    pub blake3: String,
    pub game_entry: Option<rp::GameEntry>,
    pub profile: BuildProfile,
    pub bytecode_bytes: usize,
    pub text_pool: Option<bc::TextPool>,
    pub catalogs: std::sync::Arc<reel_engine::aside::source::CatalogProducts>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BuildProfile {
    #[default]
    Debug,
    Release,
}
impl BuildProfile {
    pub fn name(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }
}

pub(crate) fn select_entry(
    checked: &Checked,
    entry: Option<&str>,
) -> Result<Option<rp::GameEntry>, Failure> {
    let Some(name) = entry else { return Ok(None) };
    let p = checked.compilation.program.package();
    let fail = |range, message| error("entry", DiagnosticCode::InvalidHir, range, message);
    let (path, short) = name
        .rsplit_once("::")
        .filter(|(_, name)| !name.is_empty())
        .ok_or_else(|| {
            fail(
                TextRange::empty(0.into()),
                "entry must name a module-level function as main::name".into(),
            )
        })?;
    let module = checked
        .modules
        .iter()
        .find(|m| m.canonical_path == path)
        .or_else(|| {
            checked
                .modules
                .iter()
                .find(|m| m.canonical_path == format!("game::{path}"))
        })
        .ok_or_else(|| {
            fail(
                TextRange::empty(0.into()),
                format!("entry module {path} was not found"),
            )
        })?;
    let at = |module: &rp::ModuleMetadata, range, message| {
        let mut failed = fail(range, message);
        let source = &p.sources[module.debug.as_ref().expect("checked source").source_id as usize];
        failed.diagnostics[0].source = Some(p.symbols[source.path.index()].clone());
        failed
    };
    let own = module
        .declarations
        .iter()
        .enumerate()
        .find(|(_, d)| d.name == short)
        .map(|(index, d)| (module, index, d));
    let found = if let Some(own) = own {
        Some(own)
    } else {
        p.modules[module.module_id as usize]
            .exports
            .iter()
            .find(|e| p.symbols[e.name.index()] == short)
            .and_then(|export| {
                let target = checked
                    .compilation
                    .program
                    .global_binding(bc::ModuleId(module.module_id), export.global)
                    .ok()?;
                checked.modules.iter().find_map(|m| {
                    m.declarations.iter().enumerate().find_map(|(index, d)| {
                        let binding = checked
                            .compilation
                            .program
                            .global_binding(bc::ModuleId(m.module_id), bc::GlobalId(d.global_id))
                            .ok()?;
                        (binding.offset == target.offset).then_some((m, index, d))
                    })
                })
            })
    };
    let (owner, index, declaration) = found.ok_or_else(|| {
        let debug = module.debug.as_ref().expect("checked source");
        at(module, text_range(debug.source_span),
            format!("story entry {name} was not found; define fn {short}(); Reel projects start main from src/main.aside"))
    })?;
    let function = &p.functions[declaration.function_id as usize];
    let signature = &p.signatures[function.signature.index()];
    if function.kind != bc::FunctionKind::Plain
        || function.context != bc::Context::Ordinary
        || !function.captures.is_empty()
        || signature.parameters.iter().any(|p| p.default.is_none())
    {
        let debug = owner.debug.as_ref().expect("checked source");
        return Err(at(owner, text_range(debug.declaration_spans[index]),
            "story entry must be an Ordinary module-level function with no required parameters or captured variables".into()));
    }
    Ok(Some(rp::GameEntry {
        module_id: owner.module_id,
        declaration_index: index as u32,
        function_id: declaration.function_id,
    }))
}

pub fn build(checked: &Checked, entry: Option<&str>) -> Result<Artifact, Failure> {
    build_with_profile(checked, entry, BuildProfile::Debug)
}

pub fn build_with_profile(
    checked: &Checked,
    entry: Option<&str>,
    profile: BuildProfile,
) -> Result<Artifact, Failure> {
    build_inner(checked, entry, profile, false)
}

pub fn build_export(
    checked: &Checked,
    entry: &str,
    profile: BuildProfile,
) -> Result<Artifact, Failure> {
    build_inner(
        checked,
        Some(entry),
        profile,
        profile == BuildProfile::Release,
    )
}

fn build_inner(
    checked: &Checked,
    entry: Option<&str>,
    profile: BuildProfile,
    external_text: bool,
) -> Result<Artifact, Failure> {
    let compiled = &checked.compilation;
    let p = compiled.program.package();
    let fail = |detail| {
        error(
            "package",
            DiagnosticCode::InvalidHir,
            TextRange::empty(0.into()),
            detail,
        )
    };
    let game_entry = select_entry(checked, entry)?;
    let mut release = (profile == BuildProfile::Release).then(|| without_debug(p));
    let mut text_pool = None;
    if let Some(release) = release.as_mut()
        && external_text
    {
        text_pool = Some(bc::externalize_text(release).map_err(|e| fail(e.to_string()))?);
    }
    let release = release
        .map(|package| bc::verify(package, bc::Limits::default()))
        .transpose()
        .map_err(|e| fail(e.to_string()))?;
    let program = release.as_ref().unwrap_or(&compiled.program);
    let p = program.package();
    let bytecode = program
        .encode(bc::Limits::default())
        .map_err(|e| fail(e.to_string()))?;
    let mut modules = checked.modules.clone();
    if profile == BuildProfile::Release {
        for module in &mut modules {
            module.debug = None;
        }
    }
    let metadata = rp::Metadata {
        schema_version: 1,
        bytecode_blake3: rp::digest(bytecode.as_bytes()),
        language_version: p.language_version,
        bytecode_version: p.format_version,
        host_abi_version: p.host_abi_version,
        content_version: p.content_version,
        modules,
        initialization_order: compiled.initialization_order.iter().map(|m| m.0).collect(),
        initializer: rp::Initializer {
            module_id: p.functions[compiled.initializer.index()].module.0,
            function_id: compiled.initializer.0,
        },
        game_entry: game_entry.clone(),
    };
    drop(release);
    let limits = rp::Limits::default();
    let bytes = rp::encode(&bytecode, &metadata, None, limits).map_err(|e| fail(e.to_string()))?;
    Ok(Artifact {
        blake3: rp::digest(&bytes),
        bytes,
        game_entry,
        profile,
        bytecode_bytes: bytecode.as_bytes().len(),
        text_pool,
        catalogs: checked.catalogs.clone(),
    })
}

fn without_debug(p: &bc::RuntimePackage) -> bc::RuntimePackage {
    // Do not clone full sources or per-instruction mappings merely to discard
    // them: large projects can hold substantially more debug data than code.
    bc::BytecodePackage {
        format_version: p.format_version,
        language_version: p.language_version,
        host_abi_version: p.host_abi_version,
        content_version: p.content_version,
        text_pool: p.text_pool,
        symbols: p.symbols.clone(),
        types: p.types.clone(),
        constants: p.constants.clone(),
        signatures: p.signatures.clone(),
        natives: p.natives.clone(),
        modules: p.modules.clone(),
        functions: p
            .functions
            .iter()
            .map(|f| bc::FunctionDef {
                module: f.module,
                name: f.name,
                kind: f.kind,
                signature: f.signature,
                context: f.context,
                captures: f.captures.clone(),
                register_count: f.register_count,
                bindings: f.bindings.clone(),
                descriptors: f.descriptors.clone(),
                code: f.code.clone(),
                regions: f.regions.clone(),
                source_map: bc::CompactSourceMap::default(),
            })
            .collect(),
        capabilities: p.capabilities.clone(),
        sources: Vec::new(),
    }
}

pub fn package_limits_json() -> Value {
    let l = rp::Limits::default();
    json!({"source_offset_bits": u32::BITS, "asb_bytes": l.max_file_bytes,
        "metadata_bytes": l.max_metadata_bytes, "sections": l.max_sections,
        "metadata_records": l.max_metadata_records, "source_symbols": l.max_source_symbols,
        "json_depth": l.max_json_depth, "json_string_bytes": l.max_json_string_bytes,
        "json_tokens": l.max_json_tokens, "asb_work": l.max_work,
        "b01": {"bytes": l.bytecode.max_bytes, "records": l.bytecode.max_records,
            "registers": l.bytecode.max_registers,
            "allocation_bytes": l.bytecode.max_allocation_bytes, "work": l.bytecode.max_work}})
}
