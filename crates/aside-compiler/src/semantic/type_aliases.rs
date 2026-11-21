//! Compile-time identities for source type aliases.
use super::*;
use crate::hir::{Form, Hir, NodeId};
use std::sync::Arc;

pub(super) fn descriptor(
    hir: &Hir,
    node: NodeId,
    module: &str,
    version: &str,
) -> Option<ExternalSymbol> {
    let Form::TypeAlias { name, .. } = hir.node(node).form else {
        return None;
    };
    let name = hir.name(name.name?);
    let mut symbol = ExternalSymbol::value(name, TypeHint::Named(format!("{module}::{name}")));
    symbol.type_only = true;
    symbol.intrinsic = Some(Intrinsic::TypeAlias);
    symbol.reference = Some(Arc::new(SymbolReference {
        module: module.into(),
        version: version.into(),
        export: Some(name.into()),
    }));
    Some(symbol)
}
