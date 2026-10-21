use aside_syntax::{GreenNodeBuilder, SyntaxKind, cursor::SyntaxNode};

fn empty(builder: &mut GreenNodeBuilder<'_>) {
    builder.start_node(SyntaxKind(1));
    builder.start_node(SyntaxKind(1));
    builder.finish_node();
    builder.finish_node();
}

#[test]
fn token_navigation_crosses_empty_subtrees_and_preserves_empty_tokens() {
    let mut builder = GreenNodeBuilder::new();
    builder.start_node(SyntaxKind(0));
    empty(&mut builder);
    builder.start_node(SyntaxKind(2));
    builder.token(SyntaxKind(3), "a");
    empty(&mut builder);
    builder.finish_node();
    empty(&mut builder);
    builder.start_node(SyntaxKind(2));
    empty(&mut builder);
    builder.token(SyntaxKind(3), "b");
    builder.finish_node();
    empty(&mut builder);
    builder.token(SyntaxKind(4), "");
    empty(&mut builder);
    builder.finish_node();
    let root = SyntaxNode::new_root(builder.finish());
    let first = root.first_token().unwrap();
    let middle = first.next_token().unwrap();
    let last = middle.next_token().unwrap();
    assert_eq!((first.text(), middle.text(), last.text()), ("a", "b", ""));
    assert_eq!(last, root.last_token().unwrap());
    assert_eq!(last.prev_token(), Some(middle.clone()));
    assert_eq!(middle.prev_token(), Some(first.clone()));
    assert!(first.prev_token().is_none());
    assert!(last.next_token().is_none());
    let children: Vec<_> = root.children().collect();
    assert!(children[0].first_token().is_none());
    assert!(children[0].last_token().is_none());
    assert_eq!(children[1].last_token(), Some(first));
    assert_eq!(children[3].first_token(), Some(middle));
}

#[test]
fn edge_tokens_walk_deep_trees_iteratively() {
    let mut builder = GreenNodeBuilder::new();
    for _ in 0..5000 {
        builder.start_node(SyntaxKind(0));
        empty(&mut builder);
    }
    builder.token(SyntaxKind(3), "value");
    for _ in 0..5000 {
        empty(&mut builder);
        builder.finish_node();
    }
    let root = SyntaxNode::new_root(builder.finish());
    assert_eq!(root.first_token().unwrap().text(), "value");
    assert_eq!(root.last_token().unwrap().text(), "value");
}
