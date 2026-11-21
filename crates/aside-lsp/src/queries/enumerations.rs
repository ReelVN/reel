//! Resolve the declaration of a case through its source enum namespace.
use super::*;

pub(super) fn definition<'a>(
    snapshot: &'a Snapshot,
    path: &str,
    name: &str,
    case: &str,
) -> Option<(EditorModule<'a>, &'a Source, NodeId, bool)> {
    let module = snapshot.project.module(path)?;
    let source = snapshot.sources.get(path)?;
    let hir = module.hir();
    let declares = |node, name| {
        hir.tokens(node)
            .any(|token| token.name.is_some_and(|id| hir.name(id) == name))
    };
    let enumeration = hir
        .children(NodeId(0))
        .map(|node| match hir.node(node).form {
            Form::Modified { declaration, .. } => declaration,
            _ => node,
        })
        .find(|&node| hir.node(node).kind == HirKind::Enum && declares(node, name))?;
    let variant = hir
        .children(enumeration)
        .find(|&node| declares(node, case))?;
    Some((module, source, variant, false))
}
