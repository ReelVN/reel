//! Source-only projections of the VM's actual bound-method declarations.
use super::*;
use crate::{
    TextRange, TextSize,
    hir::{Form, Hir, NodeId, Type, TypeId},
};
use aside_vm::{BuiltinMethod, ObjectKind};
use std::sync::{Arc, OnceLock};

/// One real bound method in the generated, read-only builtin declaration view.
/// Its symbol is metadata: ordinary member access remains a runtime operation.
#[derive(Debug)]
pub struct BuiltinMember {
    pub symbol: ExternalSymbol,
    pub definition: TextRange,
    pub documentation: &'static str,
    pub parameter_defaults: Vec<(&'static str, &'static str)>,
}

/// Methods available on one primitive receiver kind. Fixed and generic tuples
/// share this contract; a tuple-producing method promises kind, not fixed shape.
#[derive(Debug)]
pub struct BuiltinTypeMembers {
    pub receiver: TypeHint,
    pub members: Vec<BuiltinMember>,
    kind: ObjectKind,
}

/// A process-wide projection parsed once through the ordinary Aside frontend.
/// The source is an editor declaration view and is never linked or executed.
#[derive(Debug)]
pub struct BuiltinCatalog {
    pub source: Arc<str>,
    pub types: Vec<BuiltinTypeMembers>,
}

/// Get the immutable declarations used by both semantic member analysis and LSP.
pub fn builtin_catalog() -> &'static BuiltinCatalog {
    static CATALOG: OnceLock<BuiltinCatalog> = OnceLock::new();
    CATALOG.get_or_init(build)
}

/// Resolve builtin methods from semantic receiver facts, retaining useful
/// members for nullable receivers and recoverable editor analysis.
pub fn builtin_members(
    hir: &Hir,
    types: &[Type],
    ty: TypeId,
) -> Option<&'static BuiltinTypeMembers> {
    let receiver = match &types[ty.0 as usize] {
        Type::Array | Type::TypedArray(_) => ObjectKind::Array,
        Type::Table | Type::Record(_) => ObjectKind::Table,
        Type::String | Type::StringLiteral(_) => ObjectKind::String,
        Type::Tuple(_) => ObjectKind::Tuple,
        Type::Generator => ObjectKind::Generator,
        Type::Named(name) if hir.name(*name) == "Bytes" => ObjectKind::Bytes,
        Type::Named(name) if hir.name(*name) == "tuple" => ObjectKind::Tuple,
        Type::Union(members) => {
            let mut selected: Option<&BuiltinTypeMembers> = None;
            for &member in members {
                if matches!(types[member.0 as usize], Type::Null) {
                    continue;
                }
                let group = builtin_members(hir, types, member)?;
                if selected.is_some_and(|selected| !std::ptr::eq(selected, group)) {
                    return None;
                }
                selected = Some(group);
            }
            return selected;
        }
        _ => return None,
    };
    builtin_catalog()
        .types
        .iter()
        .find(|group| group.kind == receiver)
}

fn receiver_name(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Array => "array",
        ObjectKind::Tuple => "tuple",
        ObjectKind::Table => "table",
        ObjectKind::String => "string",
        ObjectKind::Bytes => "Bytes",
        ObjectKind::Generator => "generator",
        _ => unreachable!("VM builtin receiver declaration"),
    }
}

fn build() -> BuiltinCatalog {
    use std::fmt::Write;
    let mut receivers = Vec::new();
    for method in BuiltinMethod::ALL {
        for &receiver in method.declaration().receivers {
            if !receivers.contains(&receiver) {
                receivers.push(receiver);
            }
        }
    }
    let mut source = String::new();
    let mut declarations = Vec::new();
    for receiver in receivers {
        let name = receiver_name(receiver);
        writeln!(source, "class {name} {{").unwrap();
        for method in BuiltinMethod::ALL {
            let declaration = method.declaration();
            if !declaration.receivers.contains(&receiver) {
                continue;
            }
            writeln!(source, "    /// {}", declaration.documentation).unwrap();
            write!(source, "    fn ").unwrap();
            let start = TextSize::try_from(source.len()).unwrap();
            source.push_str(declaration.name);
            let definition = TextRange::new(start, TextSize::try_from(source.len()).unwrap());
            write!(source, "(self").unwrap();
            for parameter in declaration.parameters {
                write!(source, ", {}: {}", parameter.name, parameter.ty).unwrap();
                if let Some(default) = parameter.default {
                    write!(source, " = {default}").unwrap();
                }
            }
            if let Some(parameter) = declaration.rest {
                write!(source, ", ...{}: {}", parameter.name, parameter.ty).unwrap();
            }
            let result = if declaration.result == "Self" {
                name
            } else {
                declaration.result
            };
            writeln!(source, ") -> {result} {{}}\n").unwrap();
            declarations.push((receiver, *method, definition));
        }
        writeln!(source, "}}\n").unwrap();
    }
    let parsed = crate::parse(&source).expect("VM builtin declarations fit the source limit");
    assert!(
        parsed.is_ok(),
        "invalid VM builtin declarations: {:?}",
        parsed.diagnostics()
    );
    let hir = super::project::project(&parsed, AnalysisLimits::default())
        .expect("VM builtin declarations fit HIR limits");
    let mut groups: Vec<BuiltinTypeMembers> = Vec::new();
    for (receiver, method, definition) in declarations {
        let node = hir
            .nodes
            .iter()
            .enumerate()
            .find_map(|(index, node)| match node.form {
                Form::Function {
                    name: Some(name), ..
                } if name.span == definition => Some(NodeId(index as u32)),
                _ => None,
            })
            .expect("generated builtin method name");
        let mut signature = super::types::signature_hint(&hir, node).expect("builtin signature");
        signature.parameters.remove(0);
        signature.effect = CallEffect::NoWait;
        let kind = receiver;
        let receiver = super::types::builtin_type(receiver_name(kind)).unwrap();
        let group_index = groups
            .iter()
            .position(|group| group.receiver == receiver)
            .unwrap_or_else(|| {
                groups.push(BuiltinTypeMembers {
                    receiver: receiver.clone(),
                    members: Vec::new(),
                    kind,
                });
                groups.len() - 1
            });
        let declaration = method.declaration();
        let symbol =
            ExternalSymbol::function(declaration.name, signature).with_reference(SymbolReference {
                module: format!("aside::builtin::{receiver}"),
                version: "1".into(),
                export: Some(declaration.name.into()),
            });
        groups[group_index].members.push(BuiltinMember {
            symbol,
            definition,
            documentation: declaration.documentation,
            parameter_defaults: declaration
                .parameters
                .iter()
                .filter_map(|parameter| parameter.default.map(|default| (parameter.name, default)))
                .collect(),
        });
    }
    BuiltinCatalog {
        source: source.into(),
        types: groups,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_projection_preserves_every_runtime_name_default_and_bound_signature() {
        let catalog = builtin_catalog();
        assert!(std::ptr::eq(catalog, builtin_catalog()));
        let count = BuiltinMethod::ALL
            .iter()
            .map(|method| method.declaration().receivers.len())
            .sum::<usize>();
        assert_eq!(
            catalog
                .types
                .iter()
                .map(|group| group.members.len())
                .sum::<usize>(),
            count
        );
        for method in BuiltinMethod::ALL {
            let declaration = method.declaration();
            for &kind in declaration.receivers {
                let ty = super::super::types::builtin_type(receiver_name(kind)).unwrap();
                let group = catalog
                    .types
                    .iter()
                    .find(|group| group.receiver == ty)
                    .unwrap();
                let member = group
                    .members
                    .iter()
                    .find(|member| member.symbol.name == declaration.name)
                    .unwrap();
                assert_eq!(&catalog.source[member.definition], declaration.name);
                assert_eq!(member.documentation, declaration.documentation);
                let signature = member.symbol.signature.as_ref().unwrap();
                assert_eq!(
                    signature.parameters.len(),
                    declaration.parameters.len() + usize::from(declaration.rest.is_some())
                );
                for (actual, expected) in signature.parameters.iter().zip(declaration.parameters) {
                    assert_eq!(actual.name, expected.name);
                    assert_eq!(actual.default, expected.default.is_some());
                    assert!(!actual.rest);
                }
                assert_eq!(signature.effect, CallEffect::NoWait);
                assert_ne!(signature.derived_safe, Some(false));
            }
        }
    }
}
