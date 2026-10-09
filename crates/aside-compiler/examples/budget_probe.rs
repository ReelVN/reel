//! Independent process probe, also runnable against the first draft implementation.
use aside_compiler::{EntryPoint, Limits, ParseOptions, lex, lex_with_limits, parse_with_options};
fn main() {
    for budget in 0..=3 {
        let l = lex_with_limits(
            "\"a\"",
            Limits {
                max_tokens: budget,
                ..Default::default()
            },
        )
        .unwrap();
        println!(
            "max_tokens={budget},tokens={},exhausted={},codes={:?}",
            l.tokens.len(),
            l.exhausted,
            l.diagnostics
                .iter()
                .map(|d| d.code.as_str())
                .collect::<Vec<_>>()
        );
    }
    println!(
        "unicode_plus_codes={:?}",
        lex(r#""\u{+41}""#)
            .unwrap()
            .diagnostics
            .iter()
            .map(|d| d.code.as_str())
            .collect::<Vec<_>>()
    );
    let count = std::env::args().nth(1).map_or(5000, |s| s.parse().unwrap());
    let source = "if true {} else ".repeat(count) + "if true {}";
    let p = parse_with_options(
        &source,
        ParseOptions {
            entry: EntryPoint::Block,
            ..Default::default()
        },
    )
    .unwrap();
    let mut stack = vec![(p.syntax(), 1)];
    let mut deepest = 0;
    while let Some((node, depth)) = stack.pop() {
        deepest = deepest.max(depth);
        stack.extend(node.children().map(|n| (n, depth + 1)));
    }
    println!(
        "else_count={count},source_bytes={},tree_depth={deepest},exhausted={},diagnostics={:?}",
        source.len(),
        p.exhausted(),
        p.diagnostics()
            .iter()
            .map(|d| (d.code.as_str(), d.range))
            .collect::<Vec<_>>()
    );
    assert_eq!(p.syntax().text().to_string(), source);
}
