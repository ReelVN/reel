use super::AnalysisLimits;
use crate::hir::*;
use crate::{Diagnostic, DiagnosticCode as D, Parse, SyntaxKind as K, SyntaxNode};
use smallvec::SmallVec;
use std::collections::HashMap;

pub(super) fn project(parsed: &Parse, limits: AnalysisLimits) -> Result<Hir, Diagnostic> {
    project_inner(parsed, limits, false)
}

pub(super) fn project_editor(parsed: &Parse, limits: AnalysisLimits) -> Result<Hir, Diagnostic> {
    project_inner(parsed, limits, true)
}

fn project_inner(parsed: &Parse, limits: AnalysisLimits, recover: bool) -> Result<Hir, Diagnostic> {
    let syntax = parsed.syntax();
    let mut hir = Hir {
        source: syntax.text().to_string().into(),
        nodes: Vec::new(),
        elements: Vec::new(),
        names: Vec::new(),
    };
    let mut names = HashMap::new();
    append_syntax(syntax, None, 0, &mut hir, &mut names, limits, recover)?;
    super::forms::forms(&mut hir, recover);
    Ok(hir)
}

/// Dependency discovery projects only grammar-recorded imports. Full source
/// analysis separately admits every body against its complete HIR budgets.
pub(super) fn project_imports(
    parsed: &Parse,
    source: &str,
    limits: AnalysisLimits,
) -> Result<Hir, Diagnostic> {
    let root = parsed.syntax();
    if limits.max_nodes == 0 {
        return Err(Diagnostic::error(
            D::BudgetExceeded,
            root.text_range(),
            "HIR node/depth budget exceeded",
        ));
    }
    let mut hir = Hir {
        source: source.into(),
        nodes: vec![HirNode {
            kind: HirKind::Module,
            span: root.text_range(),
            parent: None,
            elements: 0..0,
            form: Form::Sequence,
        }],
        elements: Vec::new(),
        names: Vec::new(),
    };
    let mut names = HashMap::new();
    let mut imports = Vec::new();
    for &offset in parsed.import_offsets() {
        let syntax = root
            .token_at_offset(offset)
            .right_biased()
            .expect("parsed use keyword")
            .parent_ancestors()
            .find(|node| node.kind() == K::UseDecl)
            .expect("parsed import declaration");
        imports.push(HirElement::Node(append_syntax(
            syntax,
            Some(NodeId(0)),
            1,
            &mut hir,
            &mut names,
            limits,
            false,
        )?));
    }
    if hir.elements.len().saturating_add(imports.len()) > u32::MAX as usize {
        return Err(Diagnostic::error(
            D::BudgetExceeded,
            root.text_range(),
            "HIR edge budget exceeded",
        ));
    }
    let start = hir.elements.len() as u32;
    hir.elements.extend(imports);
    hir.nodes[0].elements = start..hir.elements.len() as u32;
    super::forms::forms(&mut hir, false);
    Ok(hir)
}

fn append_syntax(
    syntax: SyntaxNode,
    parent: Option<NodeId>,
    depth: usize,
    hir: &mut Hir,
    names: &mut HashMap<String, NameId>,
    limits: AnalysisLimits,
    recover: bool,
) -> Result<NodeId, Diagnostic> {
    // Explicit projection frames retain only direct edges, so a flat binary or
    // postfix chain does not consume one Rust call frame per CST node.
    struct Frame {
        syntax: SyntaxNode,
        children: aside_syntax::SyntaxElementChildren<crate::AsideLanguage>,
        id: NodeId,
        depth: usize,
        edges: SmallVec<[HirElement; 8]>,
    }
    fn enter(
        syntax: SyntaxNode,
        parent: Option<NodeId>,
        depth: usize,
        hir: &mut Hir,
        limits: AnalysisLimits,
    ) -> Result<Frame, Diagnostic> {
        if hir.nodes.len() >= limits.max_nodes.min(u32::MAX as usize)
            || depth >= limits.max_depth.max(1)
        {
            return Err(Diagnostic::error(
                D::BudgetExceeded,
                syntax.text_range(),
                "HIR node/depth budget exceeded",
            ));
        }
        let id = NodeId(hir.nodes.len() as u32);
        let kind = kind(syntax.kind()).ok_or_else(|| {
            Diagnostic::error(
                D::InvalidHir,
                syntax.text_range(),
                "error or unsupported CST cannot enter semantic analysis",
            )
        })?;
        hir.nodes.push(HirNode {
            kind,
            span: syntax.text_range(),
            parent,
            elements: 0..0,
            form: Form::Sequence,
        });
        Ok(Frame {
            children: syntax.children_with_tokens(),
            syntax,
            id,
            depth,
            edges: SmallVec::new(),
        })
    }
    let mut stack = vec![enter(syntax, parent, depth, hir, limits)?];
    let root = stack[0].id;
    while let Some(frame) = stack.last_mut() {
        if let Some(element) = frame.children.next() {
            match element {
                aside_syntax::NodeOrToken::Node(node) => {
                    if recover && node.kind() == K::Error {
                        continue;
                    }
                    let flat_receiver =
                        !frame.edges.iter().any(|e| matches!(e, HirElement::Node(_)))
                            && matches!(
                                frame.syntax.kind(),
                                K::BinaryExpr | K::MemberExpr | K::IndexExpr | K::CallExpr
                            );
                    let child = enter(
                        node,
                        Some(frame.id),
                        frame.depth + usize::from(!flat_receiver),
                        hir,
                        limits,
                    )?;
                    stack.push(child);
                }
                aside_syntax::NodeOrToken::Token(token) if !token.kind().is_trivia() => {
                    let name = if token.kind() == K::Ident {
                        let next = NameId(hir.names.len() as u32);
                        Some(if let Some(&name) = names.get(token.text()) {
                            name
                        } else {
                            hir.names.push(token.text().to_owned());
                            names.insert(token.text().to_owned(), next);
                            next
                        })
                    } else {
                        None
                    };
                    frame.edges.push(HirElement::Token(HirToken {
                        kind: token.kind(),
                        span: token.text_range(),
                        name,
                    }));
                }
                _ => (),
            }
            continue;
        }
        let Frame {
            syntax, id, edges, ..
        } = stack.pop().unwrap();
        // CST trivia belongs to adjacent nodes, but author diagnostics and
        // runtime locations begin at syntax, not the preceding blank line.
        if hir.nodes[id.0 as usize].parent.is_some()
            && let (Some(first), Some(last)) = (edges.first(), edges.last())
        {
            let span = |element: &HirElement| match element {
                HirElement::Node(id) => hir.nodes[id.0 as usize].span,
                HirElement::Token(token) => token.span,
            };
            hir.nodes[id.0 as usize].span =
                crate::TextRange::new(span(first).start(), span(last).end());
        }
        if hir.elements.len().saturating_add(edges.len()) > u32::MAX as usize {
            return Err(Diagnostic::error(
                D::BudgetExceeded,
                syntax.text_range(),
                "HIR edge budget exceeded",
            ));
        }
        let start = hir.elements.len() as u32;
        hir.elements.extend(edges);
        hir.nodes[id.0 as usize].elements = start..hir.elements.len() as u32;
        if let Some(parent) = stack.last_mut() {
            parent.edges.push(HirElement::Node(id));
        }
    }
    Ok(root)
}
fn kind(k: K) -> Option<HirKind> {
    use HirKind as H;
    Some(match k {
        K::Root => H::Module,
        K::Block => H::Block,
        K::LetDecl => H::Let,
        K::FnDecl => H::Function,
        K::ClassDecl => H::Class,
        K::EnumDecl => H::Enum,
        K::EnumVariant => H::EnumVariant,
        K::TypeAliasDecl => H::TypeAlias,
        K::UseDecl => H::Import,
        K::UsePath => H::ImportPath,
        K::UseGroup => H::ImportGroup,
        K::ParamList => H::Parameters,
        K::Param => H::Parameter,
        K::TypeName => H::NamedType,
        K::ArrayType => H::ArrayType,
        K::StringType => H::StringType,
        K::RecordType => H::RecordType,
        K::RecordFieldType => H::RecordFieldType,
        K::TupleType => H::TupleType,
        K::UnionType => H::UnionType,
        K::FunctionType => H::FunctionType,
        K::GroupType => H::GroupType,
        K::ExprStmt => H::ExpressionStatement,
        K::ReturnStmt => H::Return,
        K::BreakStmt => H::Break,
        K::ContinueStmt => H::Continue,
        K::ThrowStmt => H::Throw,
        K::YieldExpr => H::Yield,
        K::NameExpr => H::Name,
        K::LiteralExpr => H::Literal,
        K::StringExpr => H::String,
        K::Interpolation => H::Interpolation,
        K::TupleExpr => H::Tuple,
        K::GroupExpr => H::Group,
        K::ArrayExpr => H::Array,
        K::ObjectExpr => H::Object,
        K::ObjectField => H::ObjectField,
        K::Spread => H::Spread,
        K::PrefixExpr => H::Prefix,
        K::BinaryExpr => H::Binary,
        K::RangeExpr => H::Range,
        K::AssignExpr => H::Assignment,
        K::MemberExpr => H::Member,
        K::IndexExpr => H::Index,
        K::CallExpr => H::Call,
        K::ArgList => H::Arguments,
        K::Arg => H::Argument,
        K::NamedArg => H::NamedArgument,
        K::Children => H::Children,
        K::ClosureExpr => H::Closure,
        K::IfExpr => H::If,
        K::WhileStmt => H::While,
        K::ForStmt => H::For,
        K::LoopStmt => H::Loop,
        K::LabelStmt => H::Label,
        K::MatchExpr => H::Match,
        K::MatchArm => H::MatchArm,
        K::TryStmt => H::Try,
        K::CatchClause => H::Catch,
        K::FinallyClause => H::Finally,
        K::NamePattern => H::NamePattern,
        K::WildcardPattern => H::WildcardPattern,
        K::LiteralPattern => H::LiteralPattern,
        K::TuplePattern => H::TuplePattern,
        K::GroupPattern => H::GroupPattern,
        K::ArrayPattern => H::ArrayPattern,
        K::ObjectPattern => H::ObjectPattern,
        K::PatternField => H::PatternField,
        K::RestPattern => H::RestPattern,
        K::OrPattern => H::OrPattern,
        K::TypePattern => H::TypePattern,
        K::EnumPattern => H::EnumPattern,
        K::ModifiedDecl => H::ModifiedDeclaration,
        _ => return None,
    })
}
