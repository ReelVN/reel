//! Shared built-in annotation and structural signature projection.
use super::{CallEffect, ExternalParameter, ExternalSignature, TypeHint};
use crate::hir::{Form, FunctionKind, Hir, HirKind as H, NodeId};

pub(super) fn builtin_type(name: &str) -> Option<TypeHint> {
    Some(match name {
        "any" => TypeHint::Any,
        "closure" => TypeHint::Closure,
        "null" => TypeHint::Null,
        "bool" => TypeHint::Bool,
        "int" => TypeHint::Int,
        "float" => TypeHint::Float,
        "string" => TypeHint::String,
        "array" => TypeHint::Array,
        "table" | "Map" => TypeHint::Table,
        "Future" => TypeHint::Future,
        "generator" => TypeHint::Generator,
        "Bytes" | "TextBuffer" | "ByteBuffer" | "Set" | "Range" | "Iterator" | "AsyncIterator"
        | "thread" | "weakref" | "class" | "instance" => TypeHint::Named(name.into()),
        _ => return None,
    })
}

pub(super) fn signature_hint(hir: &Hir, node: NodeId) -> Option<ExternalSignature> {
    let Form::Function {
        parameters,
        result,
        kind,
        ..
    } = hir.node(node).form
    else {
        return None;
    };
    let parameters = parameters
        .into_iter()
        .flat_map(|node| hir.children(node))
        .filter_map(|node| {
            let Form::Parameter {
                name,
                annotation,
                default,
                mutable,
                rest,
            } = hir.node(node).form
            else {
                return None;
            };
            Some(ExternalParameter {
                name: hir.name(name.name?).into(),
                ty: annotation.map_or(TypeHint::Any, |node| type_hint(hir, node)),
                mutable,
                default: default.is_some(),
                rest,
            })
        })
        .collect();
    Some(ExternalSignature {
        result_cases: Vec::new(),
        parameters,
        result: match kind {
            FunctionKind::Async => TypeHint::Future,
            FunctionKind::Generator => TypeHint::Generator,
            FunctionKind::Plain => result.map_or(TypeHint::Any, |node| type_hint(hir, node)),
        },
        effect: CallEffect::Unknown,
        init_safe: None,
        derived_safe: None,
    })
}

pub(super) fn type_hint(hir: &Hir, n: NodeId) -> TypeHint {
    match hir.node(n).kind {
        H::NamedType => {
            let text = hir.tokens(n).map(|t| hir.text(t.span)).collect::<String>();
            builtin_type(&text).unwrap_or(TypeHint::Named(text))
        }
        H::GroupType => hir
            .children(n)
            .next()
            .map_or(TypeHint::Any, |n| type_hint(hir, n)),
        H::TupleType => TypeHint::Tuple(hir.children(n).map(|n| type_hint(hir, n)).collect()),
        H::UnionType => TypeHint::Union(hir.children(n).map(|n| type_hint(hir, n)).collect()),
        H::FunctionType => {
            let mut args = hir
                .children(n)
                .map(|n| type_hint(hir, n))
                .collect::<Vec<_>>();
            let result = args.pop().unwrap_or(TypeHint::Any);
            TypeHint::Callable(args, Box::new(result))
        }
        _ => TypeHint::Any,
    }
}
