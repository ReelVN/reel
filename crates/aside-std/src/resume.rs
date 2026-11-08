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
            "Name the complete interaction call immediately following this statement. Its presentation argument preparation must be read-only and non-waiting. Keys are unique within the enclosing flow.",
        ),
        (
            "return_point",
            Intrinsic::ResumeReturn,
            "Name the static flow call immediately following this statement. A restored parent continues after that call using its current result slot, without repeating the original argument expressions.",
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
        assert_eq!(module.exports.len(), 3);
        for symbol in &module.exports {
            assert!(symbol.intrinsic.is_some());
            assert_eq!(symbol.signature.as_ref().unwrap().parameters[0].name, "key");
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
