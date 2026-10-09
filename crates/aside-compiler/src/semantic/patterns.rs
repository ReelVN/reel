//! One grammar-role projection for local bindings and exported descriptors.
use crate::hir::{Form, Hir, HirKind as H, NodeId};
use smallvec::SmallVec;

pub(super) enum Shape<T> {
    Unknown,
    Tuple(Vec<T>),
    Array,
    Table,
    Other,
}
pub(super) trait PatternTypes {
    type Ty: Clone;
    fn hir(&self) -> &Hir;
    fn unknown(&self) -> Self::Ty;
    fn shape(&self, ty: &Self::Ty) -> Shape<Self::Ty>;
    fn tuple(&mut self, elements: Vec<Self::Ty>) -> Self::Ty;
    fn array(&self) -> Self::Ty;
    fn table(&self) -> Self::Ty;
    fn annotation(&mut self, node: NodeId) -> Self::Ty;
    fn compatible(&self, actual: &Self::Ty, expected: &Self::Ty) -> bool;
    fn charge(&mut self, node: NodeId) -> bool;
}
pub(super) type Projections<T> = SmallVec<[(NodeId, T); 4]>;

/// False means this alternative cannot match the known shape. It is ordinary
/// branch failure; the caller decides whether an entire required binding fails.
pub(super) fn project<C: PatternTypes>(
    context: &mut C,
    node: NodeId,
    value: C::Ty,
    output: &mut Projections<C::Ty>,
) -> bool {
    if !context.charge(node) {
        return false;
    }
    let start = output.len();
    let form = context.hir().node(node).form;
    let kind = context.hir().node(node).kind;
    let matches = match form {
        Form::Name { name } => {
            if name
                .name
                .is_some_and(|name| context.hir().name(name) != "_")
            {
                output.push((node, value));
            }
            true
        }
        Form::Single { child } => project(context, child, value, output),
        Form::Or { left, right } => {
            let left = project(context, left, value.clone(), output);
            let right = project(context, right, value, output);
            left || right
        }
        Form::TypePattern {
            pattern,
            annotation,
        } => {
            let expected = context.annotation(annotation);
            if context.compatible(&value, &expected) {
                let value = if matches!(context.shape(&value), Shape::Unknown) {
                    expected
                } else {
                    value
                };
                project(context, pattern, value, output)
            } else {
                false
            }
        }
        Form::PatternField { pattern, .. } => {
            if let Some(pattern) = pattern {
                project(context, pattern, value, output)
            } else {
                output.push((node, value));
                true
            }
        }
        Form::Sequence if kind == H::TuplePattern => {
            let children = context.hir().children(node).collect::<SmallVec<[_; 4]>>();
            let elements = match context.shape(&value) {
                Shape::Tuple(elements) => Some(elements),
                Shape::Unknown => None,
                _ => return false,
            };
            let rest = children
                .last()
                .is_some_and(|&node| context.hir().node(node).kind == H::RestPattern);
            if let Some(elements) = &elements
                && ((!rest && children.len() != elements.len())
                    || (rest && children.len() - 1 > elements.len()))
            {
                return false;
            }
            let mut matches = true;
            for (i, child) in children.into_iter().enumerate() {
                let ty = if context.hir().node(child).kind == H::RestPattern {
                    if let Some(elements) = &elements {
                        context.tuple(elements[i..].to_vec())
                    } else {
                        context.unknown()
                    }
                } else {
                    elements
                        .as_ref()
                        .and_then(|elements| elements.get(i))
                        .cloned()
                        .unwrap_or_else(|| context.unknown())
                };
                matches &= project(context, child, ty, output);
            }
            matches
        }
        Form::Sequence if matches!(kind, H::ArrayPattern | H::ObjectPattern) => {
            if !matches!(
                (kind, context.shape(&value)),
                (H::ArrayPattern, Shape::Array | Shape::Unknown)
                    | (H::ObjectPattern, Shape::Table | Shape::Unknown)
            ) {
                return false;
            }
            let children = context.hir().children(node).collect::<SmallVec<[_; 4]>>();
            let mut matches = true;
            for child in children {
                let ty = if context.hir().node(child).kind == H::RestPattern {
                    if kind == H::ArrayPattern {
                        context.array()
                    } else {
                        context.table()
                    }
                } else {
                    context.unknown()
                };
                matches &= project(context, child, ty, output);
            }
            matches
        }
        _ => true,
    };
    if !matches {
        output.truncate(start);
    }
    matches
}
