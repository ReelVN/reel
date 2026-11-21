//! Shared built-in annotation and structural signature projection.
use super::{CallEffect, ExternalParameter, ExternalSignature, TypeHint};
use crate::hir::{Form, FunctionKind, Hir, HirKind as H, NodeId};
use crate::SyntaxKind as K;

macro_rules! builtin_types {
    ($name:ident; $($($spelling:literal)|+ => $ty:expr),+ $(,)?) => {
        /// Accepted built-in annotation names.
        /// Editor completion and semantic lookup share these exact spellings.
        pub const BUILTIN_TYPE_NAMES: &[&str] = &[$($($spelling),+),+];

        pub(super) fn builtin_type($name: &str) -> Option<TypeHint> {
            Some(match $name {
                $($($spelling)|+ => $ty,)+
                _ => return None,
            })
        }
    };
}
builtin_types! {
    name;
        "any" => TypeHint::Any,
        "closure" => TypeHint::Closure,
        "null" => TypeHint::Null,
        "bool" => TypeHint::Bool,
        "int" => TypeHint::Int,
        "float" => TypeHint::Float,
        "string" => TypeHint::String,
        "array" => TypeHint::Array,
        "table" => TypeHint::Table,
        "Future" => TypeHint::Future,
        "generator" => TypeHint::Generator,
        "Bytes" | "TextBuffer" | "ByteBuffer" | "Set" | "Range" | "Iterator" | "AsyncIterator"
        | "thread" | "weakref" | "class" | "instance" | "Binding" | "tuple" => TypeHint::Named(name.into()),
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
        result_parameter: None,
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
        H::ArrayType => hir.children(n).next().map_or(TypeHint::Array, |item| {
            TypeHint::TypedArray(Box::new(type_hint(hir, item)))
        }),
        H::StringType => hir
            .children(n)
            .next()
            .and_then(|node| crate::strings::static_text(hir, node))
            .map_or(TypeHint::Any, TypeHint::StringLiteral),
        H::RecordType => TypeHint::Record(
            hir.children(n)
                .filter_map(|field| {
                    let name = hir.tokens(field).next()?.name?;
                    let mut ty = type_hint(hir, hir.children(field).next()?);
                    if hir.tokens(field).any(|token| token.kind == K::Question) {
                        ty = TypeHint::OptionalField(Box::new(ty));
                    }
                    Some((hir.name(name).into(), ty))
                })
                .collect(),
        ),
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
