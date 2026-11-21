//! Source enum descriptions compiled to existing scalar, tuple and record values.
use super::{types::type_hint, *};
use crate::hir::{Hir, HirKind, NodeId};
use std::sync::Arc;

pub(super) fn descriptor(
    hir: &Hir,
    node: NodeId,
    module: &str,
    version: &str,
) -> Option<ExternalSymbol> {
    let name = hir.tokens(node).find_map(|token| token.name)?;
    let name = hir.name(name);
    let canonical = format!("{module}::{name}");
    let mut cases = Vec::new();
    let mut static_members = Vec::new();
    for variant in hir.children(node) {
        let Some(member_name) = hir.tokens(variant).find_map(|token| token.name) else {
            continue;
        };
        let member_name = hir.name(member_name);
        let tag = format!("{canonical}::{member_name}");
        let tag_type = TypeHint::StringLiteral(tag.clone());
        let fields = hir
            .children(variant)
            .next()
            .map(|payload| type_hint(hir, payload));
        let (case, mut member) = if let Some(TypeHint::Record(mut fields)) =
            fields.filter(|value| !matches!(value, TypeHint::Record(fields) if fields.is_empty()))
        {
            let parameters = fields
                .iter()
                .map(|(name, ty)| ExternalParameter {
                    name: name.clone(),
                    ty: ty.clone(),
                    mutable: false,
                    default: false,
                    rest: false,
                })
                .collect();
            fields.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            let case = TypeHint::Tuple(vec![tag_type, TypeHint::Record(fields)]);
            let member = ExternalSymbol::function(
                member_name,
                ExternalSignature {
                    parameters,
                    result: TypeHint::Named(canonical.clone()),
                    result_cases: Vec::new(),
                    result_parameter: None,
                    effect: CallEffect::NoWait,
                    init_safe: Some(true),
                    derived_safe: Some(true),
                },
            );
            (case, member)
        } else {
            (
                tag_type.clone(),
                ExternalSymbol::value(member_name, tag_type),
            )
        };
        member.constant = Some(Arc::new(CompileConstant::Utf8(tag)));
        member.intrinsic = Some(Intrinsic::EnumCase);
        member.reference = Some(Arc::new(SymbolReference {
            module: module.into(),
            version: version.into(),
            export: Some(format!("{name}::variant::{member_name}")),
        }));
        cases.push(case);
        static_members.push(ExternalMember {
            symbol: member,
            write_type: None,
        });
    }
    let mut symbol = ExternalSymbol::value(
        name,
        TypeHint::Enum {
            name: canonical,
            cases: Arc::new(cases),
        },
    );
    symbol.class = Some(Arc::new(ExternalClass {
        bases: Vec::new(),
        members: Vec::new(),
        static_members,
    }));
    symbol.type_only = true;
    symbol.intrinsic = Some(Intrinsic::EnumType);
    Some(symbol)
}

pub(super) fn payload_type(hir: &Hir, variant: NodeId) -> Option<NodeId> {
    hir.children(variant)
        .find(|&payload| hir.node(payload).kind == HirKind::RecordType)
}
