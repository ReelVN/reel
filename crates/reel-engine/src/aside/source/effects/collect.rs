//! Discover effect entries through resolved calls while preserving ordinary lowering.
use super::*;
use aside_compiler::{lowering, semantic::SymbolReference};
use std::collections::HashSet;

/// Collect static entries referenced by this exact native declaration. The source
/// call, Function value and parameter expressions retain their normal VM lowering.
pub fn collect_effects(
    modules: &[h::AnalyzedModule],
    native: &SymbolReference,
    limits: EffectCompileLimits,
) -> Result<Vec<CompiledEffect>, EffectDiagnostic> {
    let mut entries = HashSet::new();
    let mut output = Vec::new();
    for (module_index, module) in modules.iter().enumerate() {
        let selected =
            lowering::select_branches(module, lowering::Limits::default()).map_err(|error| {
                EffectDiagnostic {
                    module: module.source_path().into(),
                    span: error.span,
                    message: error.detail,
                }
            })?;
        for (index, node) in module.hir().nodes.iter().enumerate() {
            if !selected.contains(h::NodeId(index as u32)) {
                continue;
            }
            let Form::Call {
                callee,
                arguments,
                children,
                ..
            } = node.form
            else {
                continue;
            };
            if module.canonical_reference(callee) != Some(native) {
                continue;
            }
            let error = |span, message: &str| EffectDiagnostic {
                module: module.source_path().into(),
                span,
                message: message.into(),
            };
            if children.is_some() {
                return Err(error(node.span, "effect does not accept a children block"));
            }
            let mut function = None;
            let mut positional = 0;
            for argument in arguments
                .into_iter()
                .flat_map(|arguments| module.hir().children(arguments))
            {
                let Form::Argument { name, value } = module.hir().node(argument).form else {
                    return Err(error(
                        module.hir().node(argument).span,
                        "effect requires an explicit static function argument",
                    ));
                };
                let name = if let Some(name) = name {
                    module.hir().name(name.name.unwrap())
                } else {
                    let name = ["function", "params"]
                        .get(positional)
                        .copied()
                        .unwrap_or("");
                    positional += 1;
                    name
                };
                if name == "function" && function.replace(value).is_some() {
                    return Err(error(node.span, "duplicate effect function argument"));
                }
            }
            let function =
                function.ok_or_else(|| error(node.span, "effect requires a function"))?;
            let entry =
                resolve_effect_function(modules, module_index, function).ok_or_else(|| {
                    error(
                        module.hir().node(function).span,
                        "effect requires a statically resolved function without captures",
                    )
                })?;
            if entries.insert(entry) {
                output.push(compile_effect(modules, entry, limits)?);
            }
        }
    }
    Ok(output)
}

pub(in crate::aside::source) fn collect_registered(
    modules: &[h::AnalyzedModule],
) -> Result<Vec<CompiledEffect>, EffectDiagnostic> {
    let Some(contract) = super::super::contracts()
        .iter()
        .find(|contract| contract.symbol == "reel::effect")
    else {
        return Ok(Vec::new());
    };
    let mut entries = collect_effects(
        modules,
        &contract.reference(),
        EffectCompileLimits::default(),
    )?;
    entries.extend(super::masks::collect(modules)?);
    Ok(entries)
}

pub(in crate::aside::source) fn bind_single(
    entries: Vec<CompiledEffect>,
    compiled: &lowering::Compilation,
) -> Result<EffectProducts, EffectProductError> {
    let mut products = EffectProducts::default();
    for entry in entries {
        let function = compiled
            .metadata
            .functions
            .get(entry.entry.function.0 as usize)
            .ok_or_else(|| {
                EffectProductError(
                    "effect source function is missing from lowering metadata".into(),
                )
            })?
            .output;
        let Some(function) = function else { continue };
        products.insert(&compiled.program, function, entry.program, entry.mask)?;
    }
    Ok(products)
}

pub(in crate::aside::source) fn bind_project(
    entries: Vec<CompiledEffect>,
    module_paths: &[String],
    compiled: &lowering::ProjectCompilation,
) -> Result<EffectProducts, EffectProductError> {
    let mut products = EffectProducts::default();
    for entry in entries {
        let module = compiled
            .modules
            .iter()
            .find(|module| module.metadata.canonical_path == module_paths[entry.entry.module])
            .ok_or_else(|| {
                EffectProductError("effect source module is missing from lowering metadata".into())
            })?;
        let function = module
            .metadata
            .functions
            .get(entry.entry.function.0 as usize)
            .ok_or_else(|| {
                EffectProductError(
                    "effect source function is missing from lowering metadata".into(),
                )
            })?
            .output;
        let Some(function) = function else { continue };
        products.insert(&compiled.program, function, entry.program, entry.mask)?;
    }
    Ok(products)
}
