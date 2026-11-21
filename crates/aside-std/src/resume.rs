//! Compile-time declarations for stable host-managed continuations.
use aside_host::{
    aside_compiler::semantic::{
        CallEffect, ExternalParameter, ExternalSignature, ExternalSymbol, Intrinsic,
        ModuleInterface, SymbolReference, TypeHint,
    },
    source::{Api, Declaration},
};

/// Add compiler directives without creating native handlers or executable stubs.
pub(super) fn describe(api: &mut Api) {
    let mut exports = Vec::new();
    for (name, intrinsic, documentation) in [
        (
            "flow",
            Intrinsic::ResumeFlow,
            "Declare a stable flow key as the first statement of an uncaptured story function. The compiler removes this directive and emits continuation metadata for embedding hosts.",
        ),
        (
            "point",
            Intrinsic::ResumePoint,
            "Name the complete recoverable call or await immediately following this statement, including a binding initializer. The compiler recognizes declared flow functions and typed methods, registered native waits, callable host values and await adapters. For example, point(\"media\"); await cue; uses the ordinary await operation. A restored flow call continues after the call without repeating its arguments. A waiting operation rebuilds read-only, non-waiting inputs. Keys are unique within the enclosing flow.",
        ),
    ] {
        let mut declaration = Declaration::namespace("std::resume", "1", documentation);
        declaration.symbol =
            ExternalSymbol::intrinsic(name, intrinsic).with_reference(SymbolReference {
                module: "std::resume".into(),
                version: "1".into(),
                export: Some(name.into()),
            });
        declaration.symbol.ty = TypeHint::Closure;
        declaration.symbol.signature = Some(ExternalSignature {
            parameters: vec![ExternalParameter {
                name: "key".into(),
                ty: TypeHint::String,
                mutable: false,
                default: false,
                rest: false,
            }],
            result: TypeHint::Null,
            result_cases: Vec::new(),
            result_parameter: None,
            effect: CallEffect::NoWait,
            init_safe: Some(false),
            derived_safe: Some(false),
        });
        declaration.parameter_documentation.insert(
            "key".into(),
            "A compile-time string identity retained across compatible code edits.".into(),
        );
        exports.push(declaration.symbol.clone());
        api.functions.push(declaration);
    }
    for (name, intrinsic, third, documentation) in [
        (
            "state",
            Intrinsic::ResumeState,
            Some("fields"),
            "Declare a module-level persistent class with an explicit stable key. Field keys default to their names; the literal fields map preserves identity after source renames. Explicit field initializers must be pure data recipes; effectful initialization belongs in ordinary init. Required fields without initializers are allowed. Any, array and table contents are checked when their data graph is captured. The compiler removes this declaration.",
        ),
        (
            "root",
            Intrinsic::ResumeRoot,
            None,
            "Declare the current bootstrap at module scope. Its single parameter directly names a module class and has an ordinary default value. The compiler declares the class with the Root key followed by .state by default. Use an explicit state declaration to choose its stable key or map renamed fields. New games evaluate the default normally. Loading supplies the restored State. Build current configuration, preferences and views here, and enter business code through run_flow. The compiler removes this declaration.",
        ),
    ] {
        let mut declaration = Declaration::namespace("std::resume", "1", documentation);
        declaration.symbol =
            ExternalSymbol::intrinsic(name, intrinsic).with_reference(SymbolReference {
                module: "std::resume".into(),
                version: "1".into(),
                export: Some(name.into()),
            });
        declaration.symbol.ty = TypeHint::Closure;
        let mut parameters = vec![
            ExternalParameter {
                name: "target".into(),
                ty: TypeHint::Any,
                mutable: false,
                default: false,
                rest: false,
            },
            ExternalParameter {
                name: "key".into(),
                ty: TypeHint::String,
                mutable: false,
                default: false,
                rest: false,
            },
        ];
        if let Some(third) = third {
            parameters.push(ExternalParameter {
                name: third.into(),
                ty: TypeHint::Table,
                mutable: false,
                default: true,
                rest: false,
            });
            declaration.parameter_documentation.insert(
                third.into(),
                "A literal map from current own field names to stable field keys.".into(),
            );
        }
        declaration.symbol.signature = Some(ExternalSignature {
            parameters,
            result: TypeHint::Null,
            result_cases: Vec::new(),
            result_parameter: None,
            effect: CallEffect::NoWait,
            init_safe: Some(true),
            derived_safe: Some(false),
        });
        declaration.parameter_documentation.insert(
            "target".into(),
            "The current static module class or bootstrap function.".into(),
        );
        declaration.parameter_documentation.insert(
            "key".into(),
            "An explicit stable identity preserved when source declarations move or are renamed."
                .into(),
        );
        exports.push(declaration.symbol.clone());
        api.functions.push(declaration);
    }
    api.modules.push(ModuleInterface {
        canonical_path: "std::resume".into(),
        version: "1".into(),
        exports,
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn directives_have_source_identity_without_runtime_native_entries() {
        let api = crate::api();
        let module = api
            .modules
            .iter()
            .find(|m| m.canonical_path == "std::resume")
            .unwrap();
        assert_eq!(module.exports.len(), 4);
        for symbol in &module.exports {
            assert!(symbol.intrinsic.is_some());
            let parameters = &symbol.signature.as_ref().unwrap().parameters;
            assert_eq!(
                parameters[usize::from(matches!(
                    symbol.intrinsic,
                    Some(
                        aside_host::aside_compiler::semantic::Intrinsic::ResumeState
                            | aside_host::aside_compiler::semantic::Intrinsic::ResumeRoot
                    )
                ))]
                .name,
                "key"
            );
            if symbol.name == "root" {
                assert_eq!(parameters.len(), 2);
            }
            assert!(
                !crate::contracts()
                    .iter()
                    .any(|contract| contract.reference().export
                        == symbol.reference.as_ref().unwrap().export
                        && contract.reference().module == "std::resume")
            );
        }
    }
}
