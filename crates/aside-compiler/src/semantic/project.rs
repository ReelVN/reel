use super::AnalysisLimits;
use crate::hir::*;
use crate::{Diagnostic, DiagnosticCode as D, Parse, SyntaxKind as K, SyntaxNode};
use std::collections::HashMap;

pub(super) fn project(parsed: &Parse, limits: AnalysisLimits) -> Result<Hir, Diagnostic> {
    let syntax = parsed.syntax();
    let mut hir = Hir {
        source: syntax.text().to_string().into(),
        nodes: Vec::new(),
        elements: Vec::new(),
        names: Vec::new(),
    };
    let mut names = HashMap::new();
    // Each frame holds only its direct edges. Descendant text is never cloned.
    fn visit(
        syntax: SyntaxNode,
        parent: Option<NodeId>,
        depth: usize,
        hir: &mut Hir,
        names: &mut HashMap<String, NameId>,
        limits: AnalysisLimits,
    ) -> Result<NodeId, Diagnostic> {
        if hir.nodes.len() >= limits.max_nodes.min(u32::MAX as usize)
            || depth >= limits.max_depth.clamp(1, 128)
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
        let mut edges = Vec::new();
        for element in syntax.children_with_tokens() {
            match element {
                rowan::NodeOrToken::Node(node) => edges.push(HirElement::Node(visit(
                    node,
                    Some(id),
                    depth + 1,
                    hir,
                    names,
                    limits,
                )?)),
                rowan::NodeOrToken::Token(token) if !token.kind().is_trivia() => {
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
                    edges.push(HirElement::Token(HirToken {
                        kind: token.kind(),
                        span: token.text_range(),
                        name,
                    }));
                }
                _ => (),
            }
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
        Ok(id)
    }
    visit(syntax, None, 0, &mut hir, &mut names, limits)?;
    super::forms::forms(&mut hir);
    Ok(hir)
}
fn kind(k: K) -> Option<HirKind> {
    use HirKind as H;
    Some(match k {
        K::Root => H::Module,
        K::Block => H::Block,
        K::LetDecl => H::Let,
        K::FnDecl => H::Function,
        K::ClassDecl => H::Class,
        K::UseDecl => H::Import,
        K::UsePath => H::ImportPath,
        K::UseGroup => H::ImportGroup,
        K::ParamList => H::Parameters,
        K::Param => H::Parameter,
        K::TypeName => H::NamedType,
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
        K::ModifiedDecl => H::ModifiedDeclaration,
        _ => return None,
    })
}
