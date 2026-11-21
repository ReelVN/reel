//! Syntax-aware keyword completions.
use super::*;

pub(super) fn keyword_completions(source: &Source, offset: usize) -> Vec<CompletionItem> {
    let mut token = completion_token(source, offset);
    while token.as_ref().is_some_and(|token| token.kind().is_trivia()) {
        token = token.and_then(|token| token.prev_token());
    }
    let closed_block = |node: &aside_compiler::SyntaxNode| {
        node.children_with_tokens().any(|element| {
            element.kind() == K::RBrace && u32::from(element.text_range().end()) as usize <= offset
        })
    };
    let ancestors: Vec<_> = token
        .as_ref()
        .into_iter()
        .flat_map(|token| token.parent_ancestors())
        .filter(|node| match node.kind() {
            K::Block | K::ClassDecl | K::EnumDecl => !closed_block(node),
            K::FnDecl | K::ClosureExpr => !node
                .children()
                .filter(|node| node.kind() == K::Block)
                .any(|node| closed_block(&node)),
            _ => true,
        })
        .collect();
    if ancestors
        .iter()
        .any(|node| matches!(node.kind(), K::UseDecl | K::MemberExpr))
    {
        return Vec::new();
    }
    let previous_token = previous_completion_token(source, offset);
    let previous = previous_token.as_ref().map(|token| token.kind());
    if matches!(previous, Some(K::Dot | K::QuestionDot | K::ColonColon)) {
        return Vec::new();
    }
    let class_body = ancestors
        .iter()
        .find(|node| matches!(node.kind(), K::Block | K::ClassDecl))
        .is_some_and(|node| {
            node.kind() == K::ClassDecl
                && node.children_with_tokens().any(|element| {
                    element.kind() == K::LBrace
                        && (u32::from(element.text_range().start()) as usize) < offset
                })
        });
    let type_position = type_completion_context(source, offset);
    let module_scope = !ancestors
        .iter()
        .any(|node| matches!(node.kind(), K::Block | K::ClassDecl | K::EnumDecl));
    let function_boundary = ancestors
        .iter()
        .position(|node| matches!(node.kind(), K::FnDecl | K::ClosureExpr));
    let in_loop = ancestors
        .iter()
        .take(function_boundary.unwrap_or(ancestors.len()))
        .any(|node| matches!(node.kind(), K::WhileStmt | K::ForStmt | K::LoopStmt));
    let generator = function_boundary.is_some_and(|index| {
        ancestors[index]
            .children_with_tokens()
            .any(|element| element.kind() == K::Gen)
    });
    let synchronous_region = ancestors
        .iter()
        .take(function_boundary.unwrap_or(ancestors.len()))
        .any(|node| matches!(node.kind(), K::FinallyClause | K::Param));
    let followup = previous_token
        .as_ref()
        .filter(|token| token.kind() == K::RBrace)
        .and_then(|token| token.parent())
        .filter(|node| node.kind() == K::Block)
        .and_then(|node| node.parent())
        .map(|node| node.kind());
    let for_pattern = previous_token.as_ref().is_some_and(|token| {
        token
            .parent_ancestors()
            .take_while(|node| node.kind() != K::ForStmt)
            .any(|node| node.kind() == K::NamePattern)
    }) && ancestors.iter().any(|node| node.kind() == K::ForStmt);
    let expression = matches!(
        previous,
        Some(
            K::Eq
                | K::LParen
                | K::LBracket
                | K::Comma
                | K::Colon
                | K::Return
                | K::Throw
                | K::Yield
                | K::If
                | K::While
                | K::Match
                | K::Await
                | K::FatArrow
                | K::Plus
                | K::Minus
                | K::Star
                | K::Slash
                | K::AndAnd
                | K::OrOr
                | K::QuestionQuestion
        )
    );
    let mut items: Vec<_> = K::KEYWORDS
        .iter()
        .filter(|(kind, _)| match previous {
            _ if *kind == K::Async => false,
            _ if *kind == K::Enum && !module_scope => false,
            _ if *kind == K::Yield && (!generator || synchronous_region) => false,
            _ if *kind == K::Await && (generator || synchronous_region) => false,
            Some(K::Let) => *kind == K::Mut,
            Some(K::Fn | K::Class | K::Enum | K::As | K::Mut) => false,
            _ if *kind == K::Else && !expression && !type_position => followup == Some(K::IfExpr),
            _ if *kind == K::Catch && !expression && !type_position => followup == Some(K::TryStmt),
            _ if *kind == K::Finally && !expression && !type_position => {
                matches!(followup, Some(K::TryStmt | K::CatchClause))
            }
            _ if for_pattern => *kind == K::In,
            Some(K::Async | K::Gen) => *kind == K::Fn,
            Some(K::Static) => matches!(kind, K::Let | K::Fn | K::Async | K::Gen),
            Some(K::Pub) => matches!(
                kind,
                K::Let | K::Fn | K::Class | K::Enum | K::Use | K::Async | K::Gen
            ),
            _ if type_position => matches!(kind, K::Fn | K::Null),
            _ if class_body => matches!(kind, K::Let | K::Fn | K::Static | K::Async | K::Gen),
            _ if expression => matches!(
                kind,
                K::Fn
                    | K::Class
                    | K::Async
                    | K::Gen
                    | K::If
                    | K::Match
                    | K::Await
                    | K::True
                    | K::False
                    | K::Null
                    | K::Yield
            ),
            _ if matches!(kind, K::Break | K::Continue) => in_loop,
            _ if *kind == K::Yield => generator,
            _ if matches!(kind, K::Return | K::Await) => function_boundary.is_some(),
            _ => !matches!(
                kind,
                K::Mut | K::Static | K::As | K::In | K::Is | K::Else | K::Catch | K::Finally
            ),
        })
        .map(|(_, name)| CompletionItem {
            label: (*name).into(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        })
        .collect();
    if module_scope
        && !type_position
        && !expression
        && !matches!(
            previous,
            Some(
                K::Let
                    | K::Mut
                    | K::Fn
                    | K::Class
                    | K::Enum
                    | K::As
                    | K::Static
                    | K::Async
                    | K::Gen
            )
        )
    {
        items.push(CompletionItem {
            label: "type".into(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        });
    }
    items
}
