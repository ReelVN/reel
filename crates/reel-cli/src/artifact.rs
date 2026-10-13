use super::{Checked, Failure, error};
use aside_compiler::{DiagnosticCode, TextRange, hir as h, lowering::Compilation};
use reel_package as rp;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) fn declarations(
    module: &h::AnalyzedModule,
    compilation: &Compilation,
) -> Result<Vec<rp::Declaration>, Failure> {
    let globals: BTreeMap<_, _> = compilation
        .metadata
        .globals
        .iter()
        .map(|g| (g.source, g.global))
        .collect();
    let mut declarations = Vec::new();
    for (source_function, function) in module.functions().iter().enumerate() {
        let Some(symbol_id) = module.facts()[function.node.0 as usize].symbol else {
            continue;
        };
        let symbol = &module.symbols()[symbol_id.0 as usize];
        if symbol.kind != h::SymbolKind::Function || symbol.origin != h::BindingOrigin::ModuleGlobal
        {
            continue;
        }
        let global = globals.get(&symbol_id).ok_or_else(|| {
            error(
                "metadata",
                DiagnosticCode::InvalidHir,
                symbol.declaration,
                "module function lacks compiler global mapping".into(),
            )
        })?;
        let output = compilation
            .metadata
            .functions
            .get(source_function)
            .ok_or_else(|| {
                error(
                    "metadata",
                    DiagnosticCode::InvalidHir,
                    symbol.declaration,
                    "module function lacks compiler function mapping".into(),
                )
            })?;
        declarations.push(rp::Declaration {
            name: module.hir().name(symbol.name).into(),
            source_symbol_id: symbol_id.0,
            source_function_id: source_function as u32,
            global_id: global.0,
            function_id: output.output.0,
            declaration_span: span(symbol.declaration),
        });
    }
    Ok(declarations)
}
fn span(range: TextRange) -> rp::Span {
    rp::Span {
        start: range.start().into(),
        end: range.end().into(),
    }
}
fn text_range(span: rp::Span) -> TextRange {
    TextRange::new(span.start.into(), span.end.into())
}

pub struct Artifact {
    pub bytes: Vec<u8>,
    /// Full container digest is the artifact identity, not content_version=1.
    pub blake3: String,
    pub game_entry: Option<rp::GameEntry>,
}

pub(crate) fn select_entry(
    checked: &Checked,
    entry: Option<&str>,
) -> Result<Option<rp::GameEntry>, Failure> {
    let compiled = &checked.compilation;
    let m = &compiled.metadata;
    let p = compiled.program.package();
    let fail = |stage, range, detail| error(stage, DiagnosticCode::InvalidHir, range, detail);
    let game_entry = if let Some(name) = entry {
        let prefix = format!("{}::", m.canonical_path);
        let short = name
            .strip_prefix(&prefix)
            .filter(|s| !s.is_empty() && !s.contains("::"))
            .ok_or_else(|| {
                fail(
                    "entry",
                    m.source_span,
                    format!("entry must name a module-level function as {prefix}name"),
                )
            })?;
        let (index, declaration) = checked
            .declarations
            .iter()
            .enumerate()
            .find(|(_, d)| d.name == short)
            .ok_or_else(|| {
                fail(
                    "entry",
                    m.source_span,
                    format!("story entry {name} was not found; define fn {short}() or select another function with --entry main::name"),
                )
            })?;
        let function = &p.functions[declaration.function_id as usize];
        let signature = &p.signatures[function.signature.index()];
        if function.kind != aside_vm::bytecode::FunctionKind::Plain
            || function.context != aside_vm::bytecode::Context::Ordinary
            || !function.captures.is_empty()
            || signature.parameters.iter().any(|p| p.default.is_none())
        {
            return Err(fail("entry", text_range(declaration.declaration_span), "story entry must be an Ordinary module-level function with no required parameters or captured variables".into()));
        }
        Some(rp::GameEntry {
            module_id: function.module.0,
            declaration_index: index as u32,
            function_id: declaration.function_id,
        })
    } else {
        None
    };
    Ok(game_entry)
}

pub fn build(checked: &Checked, entry: Option<&str>) -> Result<Artifact, Failure> {
    let compiled = &checked.compilation;
    let m = &compiled.metadata;
    let p = compiled.program.package();
    let fail = |stage, range, detail| error(stage, DiagnosticCode::InvalidHir, range, detail);
    let game_entry = select_entry(checked, entry)?;
    let module_id = p.functions[compiled.entry.index()].module.0;
    let metadata = rp::Metadata {
        schema_version: 1,
        bytecode_blake3: rp::digest(&checked.bytes),
        language_version: p.language_version,
        bytecode_version: p.format_version,
        host_abi_version: p.host_abi_version,
        content_version: p.content_version,
        module_id,
        canonical_path: m.canonical_path.clone(),
        source_version: m.source_version.clone(),
        source_blake3: rp::digest(p.sources[0].text.as_bytes()),
        source_id: 0,
        source_span: span(m.source_span),
        symbol_count: m.symbol_count,
        initializer: rp::Initializer {
            module_id,
            function_id: compiled.entry.0,
        },
        game_entry: game_entry.clone(),
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
        declarations: checked.declarations.clone(),
    };
    let limits = rp::Limits::default();
    let bytes = rp::encode(&checked.bytes, &metadata, limits)
        .map_err(|e| fail("package", m.source_span, e.to_string()))?;
    let verified =
        rp::decode(&bytes, limits).map_err(|e| fail("package", m.source_span, e.to_string()))?;
    if verified.metadata() != &metadata || verified.program().package() != p {
        return Err(fail(
            "package",
            m.source_span,
            "ASB round trip changed program/metadata".into(),
        ));
    }
    Ok(Artifact {
        blake3: rp::digest(&bytes),
        bytes,
        game_entry,
    })
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
