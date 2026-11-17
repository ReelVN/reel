//! One-time grammar interpretation. Downstream lowering consumes Forms and
//! packed sequence edges rather than rediscovering operators/roles from tokens.
use crate::{SyntaxKind as K, hir::*};
use smallvec::SmallVec;
pub(super) fn forms(hir: &mut Hir, recover: bool) {
    for index in (0..hir.nodes.len()).rev() {
        let n = NodeId(index as u32);
        hir.nodes[index].form = match form(hir, n) {
            Some(form) => form,
            None if recover => {
                hir.nodes[index].kind = HirKind::Error;
                Form::Sequence
            }
            None => panic!("valid syntax projects a complete grammar form"),
        };
    }
}
fn form(hir: &Hir, n: NodeId) -> Option<Form> {
    let children = hir.children(n).collect::<SmallVec<[_; 4]>>();
    let tokens = hir.tokens(n).collect::<SmallVec<[_; 4]>>();
    let token = |kind| tokens.iter().find(|t| t.kind == kind).copied();
    let name = || token(K::Ident);
    let has = |kind| token(kind).is_some();
    let pattern = || {
        children
            .iter()
            .copied()
            .find(|&c| is_pattern(hir.node(c).kind))
    };
    let annotation = || {
        children
            .iter()
            .copied()
            .find(|&c| is_type(hir.node(c).kind))
    };
    let body = || children.last().copied();
    use HirKind as H;
    Some(match hir.node(n).kind {
        H::Error
        | H::Module
        | H::Array
        | H::Tuple
        | H::Object
        | H::String
        | H::Parameters
        | H::Arguments
        | H::Children
        | H::ImportGroup
        | H::TupleType
        | H::RecordType
        | H::UnionType
        | H::TuplePattern
        | H::ArrayPattern
        | H::ObjectPattern => Form::Sequence,
        H::Block => Form::Block {
            tail: children.last().copied().filter(|&c| {
                matches!(
                    hir.node(c).form,
                    Form::Statement {
                        terminated: false,
                        ..
                    }
                ) || matches!(hir.node(c).kind, H::If | H::Match)
            }),
        },
        H::Let => Form::Binding {
            pattern: pattern()?,
            annotation: annotation(),
            initializer: children
                .iter()
                .copied()
                .find(|&c| !is_pattern(hir.node(c).kind) && !is_type(hir.node(c).kind)),
            mutable: has(K::Mut),
        },
        H::Function | H::Closure => Form::Function {
            name: name(),
            parameters: children
                .iter()
                .copied()
                .find(|&c| hir.node(c).kind == H::Parameters),
            result: annotation(),
            body: body()?,
            kind: if has(K::Async) {
                FunctionKind::Async
            } else if has(K::Gen) {
                FunctionKind::Generator
            } else {
                FunctionKind::Plain
            },
        },
        H::Class => Form::Class {
            name: name(),
            base: children.iter().copied().find(|&c| {
                !matches!(
                    hir.node(c).kind,
                    H::Let | H::Function | H::ModifiedDeclaration
                )
            }),
        },
        H::Parameter => Form::Parameter {
            name: name()?,
            annotation: annotation(),
            default: children
                .iter()
                .copied()
                .find(|&c| !is_type(hir.node(c).kind)),
            mutable: has(K::Mut),
            rest: has(K::Ellipsis),
        },
        H::Import => Form::Import { path: body()? },
        H::ImportPath => Form::ImportPath {
            alias: tokens
                .iter()
                .position(|t| t.kind == K::As)
                .and_then(|i| tokens.get(i + 1).copied()),
            group: children.first().copied(),
            wildcard: has(K::Star),
        },
        H::NamedType => Form::TypeName,
        H::FunctionType => Form::FunctionType { result: body()? },
        H::GroupType
        | H::StringType
        | H::RecordFieldType
        | H::Group
        | H::GroupPattern
        | H::Interpolation => Form::Single { child: body()? },
        H::ExpressionStatement => Form::Statement {
            expression: body()?,
            terminated: has(K::Semi),
        },
        H::Return | H::Throw | H::Yield => Form::Transfer {
            value: children.first().copied(),
            label: None,
        },
        H::Break | H::Continue => Form::Transfer {
            value: None,
            label: name(),
        },
        H::Name | H::NamePattern | H::RestPattern => Form::Name { name: name()? },
        H::WildcardPattern => Form::Wildcard,
        H::Literal => Form::Literal {
            token: tokens.first().copied()?,
        },
        H::LiteralPattern => Form::LiteralPattern {
            value: body()?,
            negative: has(K::Minus),
        },
        H::ObjectField => {
            let value = body()?;
            let key = if let Some(name) = name() {
                FieldKey::Name(name)
            } else if tokens.is_empty()
                && let Form::Name { name } = hir.node(value).form
            {
                FieldKey::Name(name)
            } else {
                FieldKey::Expression(*children.first()?)
            };
            Form::Field { key, value }
        }
        H::Spread => Form::Spread { value: body()? },
        H::Prefix => Form::Unary {
            operator: tokens.first().copied()?,
            value: body()?,
        },
        H::Binary | H::Range | H::Assignment => Form::Binary {
            left: *children.first()?,
            operator: tokens.first().copied()?,
            right: *children.get(1)?,
        },
        H::Member => Form::Member {
            object: *children.first()?,
            key: name()?,
            optional: has(K::QuestionDot),
            chain: has(K::QuestionDot) || chain(hir, children[0]),
        },
        H::Index => Form::Index {
            object: *children.first()?,
            index: *children.get(1)?,
            optional: has(K::QuestionBracket),
            chain: has(K::QuestionBracket) || chain(hir, children[0]),
        },
        H::Call => {
            let trailing = children
                .iter()
                .copied()
                .find(|&c| hir.node(c).kind == H::Children);
            if trailing.is_some()
                && let Form::Call {
                    callee,
                    arguments,
                    optional,
                    chain,
                    ..
                } = hir.node(*children.first()?).form
            {
                return Some(Form::Call {
                    callee,
                    arguments,
                    children: trailing,
                    optional,
                    chain,
                });
            }
            Form::Call {
                callee: *children.first()?,
                arguments: children
                    .iter()
                    .copied()
                    .find(|&c| hir.node(c).kind == H::Arguments),
                children: trailing,
                optional: has(K::QuestionParen),
                chain: has(K::QuestionParen) || chain(hir, children[0]),
            }
        }
        H::Argument | H::NamedArgument => Form::Argument {
            name: if hir.node(n).kind == H::NamedArgument {
                name()
            } else {
                None
            },
            value: body()?,
        },
        H::If | H::While => {
            // Grammar order is [pattern?], condition, then, [else]. An If or
            // Block expression is itself a legal condition.
            let first = usize::from(pattern().is_some());
            Form::Condition {
                pattern: pattern(),
                condition: *children.get(first)?,
                body: *children.get(first + 1)?,
                otherwise: children.get(first + 2).copied(),
            }
        }
        H::For => Form::For {
            pattern: *children.first()?,
            source: *children.get(1)?,
            body: *children.get(2)?,
            asynchronous: has(K::Await),
        },
        H::Loop => Form::Single { child: body()? },
        H::Label => Form::Label {
            name: name()?,
            body: body()?,
        },
        H::Match => Form::Match {
            value: *children.first()?,
        },
        H::MatchArm => Form::MatchArm {
            pattern: *children.first()?,
            guard: if has(K::If) {
                children.get(1).copied()
            } else {
                None
            },
            body: body()?,
        },
        H::Try => Form::Try {
            body: *children.first()?,
            catch: children
                .iter()
                .copied()
                .find(|&c| hir.node(c).kind == H::Catch),
            finally: children
                .iter()
                .copied()
                .find(|&c| hir.node(c).kind == H::Finally),
        },
        H::Catch => Form::Catch {
            name: name()?,
            body: body()?,
        },
        H::Finally => Form::Single { child: body()? },
        H::PatternField => Form::PatternField {
            key: name()?,
            pattern: pattern(),
            default: children
                .iter()
                .copied()
                .find(|&c| !is_pattern(hir.node(c).kind)),
        },
        H::OrPattern => Form::Or {
            left: *children.first()?,
            right: *children.get(1)?,
        },
        H::TypePattern => Form::TypePattern {
            pattern: *children.first()?,
            annotation: *children.get(1)?,
        },
        H::ModifiedDeclaration => Form::Modified {
            declaration: body()?,
            public: has(K::Pub),
            static_member: has(K::Static),
        },
    })
}
fn chain(hir: &Hir, n: NodeId) -> bool {
    match hir.node(n).form {
        Form::Member { chain, .. } | Form::Index { chain, .. } | Form::Call { chain, .. } => chain,
        _ => false,
    }
}
fn is_pattern(k: HirKind) -> bool {
    matches!(
        k,
        HirKind::NamePattern
            | HirKind::WildcardPattern
            | HirKind::LiteralPattern
            | HirKind::TuplePattern
            | HirKind::GroupPattern
            | HirKind::ArrayPattern
            | HirKind::ObjectPattern
            | HirKind::PatternField
            | HirKind::RestPattern
            | HirKind::OrPattern
            | HirKind::TypePattern
    )
}
fn is_type(k: HirKind) -> bool {
    matches!(
        k,
        HirKind::NamedType
            | HirKind::StringType
            | HirKind::RecordType
            | HirKind::GroupType
            | HirKind::TupleType
            | HirKind::UnionType
            | HirKind::FunctionType
    )
}
