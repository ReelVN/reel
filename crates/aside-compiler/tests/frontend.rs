use aside_compiler::{
    DiagnosticCode as D, EntryPoint as E, Limits, Parse, ParseOptions, SourceMap, SyntaxKind as K,
    SyntaxNode, byte_range, lex, parse_with_options,
};
fn at(source: &str, entry: E) -> Parse {
    parse_with_options(
        source,
        ParseOptions {
            entry,
            ..Default::default()
        },
    )
    .unwrap()
}
fn ok(source: &str, entry: E) -> Parse {
    let p = at(source, entry);
    assert!(p.is_ok(), "{source}: {:?}", p.diagnostics());
    assert_eq!(p.syntax().text().to_string(), source);
    p
}
fn has(p: &Parse, kind: K) -> usize {
    p.syntax()
        .descendants()
        .filter(|n| n.kind() == kind)
        .count()
}
fn node(p: &Parse, kind: K) -> SyntaxNode {
    p.syntax().descendants().find(|n| n.kind() == kind).unwrap()
}
fn rejects(source: &str, entry: E, code: D) -> Parse {
    let p = at(source, entry);
    assert!(
        p.diagnostics().iter().any(|d| d.code == code),
        "{source}: {:?}",
        p.diagnostics()
    );
    p
}
fn ranges(p: &Parse, source: &str) {
    let leaves: Vec<_> = p
        .syntax()
        .descendants_with_tokens()
        .filter_map(|x| x.into_token())
        .collect();
    let mut offset = 0;
    for leaf in leaves {
        let range = byte_range(leaf.text_range());
        assert_eq!(range.start, offset);
        assert!(source.is_char_boundary(range.start) && source.is_char_boundary(range.end));
        assert_eq!(&source[range.clone()], leaf.text());
        offset = range.end;
    }
    assert_eq!(offset, source.len());
    for d in p.diagnostics() {
        for r in std::iter::once(d.range).chain(d.related.iter().map(|r| r.range)) {
            let r = byte_range(r);
            assert!(r.end <= source.len());
            assert!(source.is_char_boundary(r.start) && source.is_char_boundary(r.end));
        }
    }
}
#[test]
fn tuple_group_object_block_and_spread_have_distinct_trees() {
    for (s, kind, count) in [
        ("()", K::TupleExpr, 1),
        ("(1,)", K::TupleExpr, 1),
        ("(1, 2)", K::TupleExpr, 1),
        ("(1)", K::GroupExpr, 1),
    ] {
        let p = ok(s, E::Expression);
        assert_eq!(has(&p, kind), count);
    }
    let p = ok(
        "({...base, [key + 1]: [first, ...items], name: \"中文\"})",
        E::Expression,
    );
    assert_eq!(has(&p, K::ObjectExpr), 1);
    assert_eq!(has(&p, K::Spread), 2);
    assert_eq!(has(&p, K::IndexExpr), 0);
    ok("({name: 1}); { let x = 2; x; }", E::Module);
    rejects("{name: 1};", E::Module, D::MissingSemicolon);
    let p = ok("|| ({name: 1})", E::Expression);
    assert_eq!(has(&p, K::ObjectExpr), 1);
    assert_eq!(has(&p, K::Block), 0);
    let p = ok("|| { let x = {}; x }", E::Expression);
    assert_eq!(has(&p, K::Block), 1);
    assert_eq!(has(&p, K::ObjectExpr), 1);
}
#[test]
fn closure_or_and_control_heads_use_one_expression_grammar() {
    let p = ok("a || b && c", E::Expression);
    assert_eq!(has(&p, K::ClosureExpr), 0);
    let p = ok("a || || true", E::Expression);
    assert_eq!(has(&p, K::ClosureExpr), 1);
    let p = ok("|x, y| x | y", E::Expression);
    assert_eq!(has(&p, K::ClosureExpr), 1);
    let p = ok(
        "if ready() { go(); } while ready() { go(); } for x in source() { x; } match source() { _ => null, }",
        E::Block,
    );
    assert_eq!(has(&p, K::Children), 0);
    assert_eq!(has(&p, K::Block), 4);
    let p = ok("if (ready() { node; }) { go(); }", E::Block);
    assert_eq!(has(&p, K::Children), 1);
    let p = ok("component(children: nodes)", E::Expression);
    assert_eq!(has(&p, K::NamedArg), 1);
    rejects(
        "component(children: nodes) { node; }",
        E::Expression,
        D::ChildrenConflict,
    );
    rejects("f(x: 1, 2)", E::Expression, D::InvalidArgumentOrder);
    let p = rejects("f(x: 1, x: 2)", E::Expression, D::DuplicateArgument);
    let d = p
        .diagnostics()
        .iter()
        .find(|d| d.code == D::DuplicateArgument)
        .unwrap();
    assert_eq!(&"f(x: 1, x: 2)"[byte_range(d.range)], "x");
    assert_eq!(d.related.len(), 1);
}
#[test]
fn exact_operator_precedence_associativity_and_nonassociative_groups() {
    let p = ok("a + b * c", E::Expression);
    let top = node(&p, K::BinaryExpr);
    assert_eq!(top.children().last().unwrap().text().to_string(), "b * c");
    let p = ok("a ?? b ?? c", E::Expression);
    let top = node(&p, K::BinaryExpr);
    assert_eq!(top.children().last().unwrap().text().to_string(), "b ?? c");
    let p = ok("a - b - c", E::Expression);
    let top = node(&p, K::BinaryExpr);
    assert_eq!(top.children().next().unwrap().text().to_string(), "a - b");
    let p = ok("-a.b(1)[0] * 2", E::Expression);
    assert_eq!(node(&p, K::PrefixExpr).text().to_string(), "-a.b(1)[0]");
    let p = ok("-1.value", E::Expression);
    assert_eq!(
        node(&p, K::PrefixExpr).children().next().unwrap().kind(),
        K::MemberExpr
    );
    ok(
        "(await f() + 1 << 2 & 3 ^ 4 | 5 < 6) == true && a || b ?? c .. d",
        E::Expression,
    );
    rejects("a < b < c", E::Expression, D::ChainedComparison);
    rejects("a == b == c", E::Expression, D::ChainedComparison);
    rejects("a .. b .. c", E::Expression, D::ChainedRange);
    ok("(a < b) == c", E::Expression);
    ok("a .. (b .. c)", E::Expression);
    ok("a = 1; a.b += 2; a[0] <<= 3;", E::Module);
    rejects("1 = 2;", E::Module, D::InvalidAssignment);
    rejects("let x = (a = 1);", E::Module, D::InvalidAssignment);
    rejects("let x = if ready { 1 };", E::Module, D::MissingElse);
    ok(
        "let x = if ready { 1 } else if other { 2 } else { 3 };",
        E::Module,
    );
}
#[test]
fn declarations_patterns_imports_loops_and_exceptions_are_real_nodes() {
    let source = r#"pub use reel::{stage as scene, ui::{text, button},};
use game::main as entry;
use reel::prelude::*;
class Child: Parent {
    let id: int;
    let mut data = [];
    static let count = 0;
    static fn make(x) { Child(x) }
    fn init(self, mut x: int = 1, ...rest) { self.id = x; }
}
async fn load() -> (string, int|null) { await read() }
gen fn events() { let incoming = yield (1, 2); yield incoming; return (); }
fn run() {
    let (first, [_, ...rest]) = data;
    let {name, score: points = 0, ...other} = event;
    outer: while let (x, y) = pair {
        for await item in stream { if ready { continue outer; } break; }
    }
    loop { break; }
    try { throw problem; } catch error { throw; } finally { cleanup(); }
    match event { {kind: "click", position: (x,y)} if x >= 0 => hit(x,y), _ => null, }
}"#;
    let p = ok(source, E::Module);
    for k in [
        K::UseGroup,
        K::ClassDecl,
        K::FnDecl,
        K::TupleType,
        K::UnionType,
        K::YieldExpr,
        K::TuplePattern,
        K::ArrayPattern,
        K::ObjectPattern,
        K::RestPattern,
        K::LabelStmt,
        K::WhileStmt,
        K::ForStmt,
        K::LoopStmt,
        K::TryStmt,
        K::CatchClause,
        K::FinallyClause,
        K::MatchArm,
    ] {
        assert!(has(&p, k) > 0, "missing {k:?}");
    }
    rejects("fn f(...rest, x) {}", E::Module, D::InvalidRest);
    rejects("let [a, ...rest, b] = xs;", E::Module, D::InvalidRest);
    rejects("try {}", E::Module, D::ExpectedToken);
}
#[test]
fn all_priority_pairs_are_exercised_with_tree_nesting() {
    let groups = ["??", "||", "&&", "==", "<", "|", "^", "&", "<<", "+", "*"];
    for pair in groups.windows(2) {
        let source = format!("a {} b {} c", pair[0], pair[1]);
        let p = if pair == ["==", "<"] {
            rejects(&source, E::Expression, D::ChainedComparison)
        } else {
            ok(&source, E::Expression)
        };
        let outer = node(&p, K::BinaryExpr);
        assert_eq!(
            outer.children().last().unwrap().text().to_string(),
            format!("b {} c", pair[1])
        );
    }
    let p = ok("a?.b?[index]?(named: value).c", E::Expression);
    assert_eq!(has(&p, K::MemberExpr), 2);
    assert_eq!(has(&p, K::IndexExpr), 1);
    assert_eq!(has(&p, K::CallExpr), 1);
}
#[test]
fn strings_comments_unicode_and_exact_ranges_roundtrip() {
    let source = "// 中文\r\n/* 外 /* 内 */ 层 */ let 名字 = f\"值：{({key: r\"\\n\"})[key]} {{字}}\"; let 文 = \"\"\"首行\n次行\"\"\";";
    let p = ok(source, E::Module);
    ranges(&p, source);
    assert_eq!(has(&p, K::Interpolation), 1);
    assert_eq!(
        p.syntax()
            .descendants_with_tokens()
            .filter_map(|x| x.into_token())
            .filter(|t| t.kind() == K::BlockComment)
            .count(),
        1
    );
    let source = "let 中文 = \"\\u{D800}\";\nlet 下一 = 1;";
    let p = rejects(source, E::Module, D::InvalidUnicodeEscape);
    ranges(&p, source);
    let d = p
        .diagnostics()
        .iter()
        .find(|d| d.code == D::InvalidUnicodeEscape)
        .unwrap();
    assert_eq!(&source[byte_range(d.range)], "\\u{D800}");
    assert_eq!(has(&p, K::LetDecl), 2);
    let position = SourceMap::new(source)
        .position(usize::from(d.range.start()))
        .unwrap();
    assert_eq!(
        (
            position.line,
            position.byte_column,
            position.scalar_column,
            position.utf16_column
        ),
        (0, 14, 10, 10)
    );
    assert!(SourceMap::new("中").position(1).is_none());
    let m = SourceMap::new("中😀\r\nx");
    assert_eq!(m.position(7).unwrap().utf16_column, 3);
    assert_eq!(m.position(9).unwrap().line, 1);
    for s in [
        r#""\u{110000}""#,
        r#""\u{12G4}""#,
        r#""\u{}""#,
        r#""\u{1234567}""#,
    ] {
        let p = rejects(s, E::Expression, D::InvalidUnicodeEscape);
        ranges(&p, s);
    }
    for s in [r#""\q""#, "\"\\", "\"x\n"] {
        let p = at(s, E::Expression);
        assert!(!p.is_ok());
        ranges(&p, s);
    }
    ok(r#""\n\r\t\0\\\"\u{1f600}""#, E::Expression);
    ok(r#"f"{f"{name}"} {{literal}}""#, E::Expression);
    ok("r\"\"\"中文\\n\n{}\"\"\"", E::Expression);
    rejects("f\"}text\"", E::Expression, D::InvalidInterpolation);
    let p = at("let 😀 = 1;", E::Module);
    ranges(&p, "let 😀 = 1;");
}
#[test]
fn numeric_boundaries_include_minimum_and_malformed_exponents() {
    for s in [
        "0",
        "9_223_372_036_854_775_807",
        "-9223372036854775808",
        "-0x8000000000000000",
        "0x7fff_ffff_ffff_ffff",
        "0o17",
        "0b1010",
        "1.25",
        "1e-3",
        "1.0e+3",
        "1e-9999",
    ] {
        ok(s, E::Expression);
    }
    for s in [
        "9223372036854775808",
        "18446744073709551616",
        "0x8000000000000001",
    ] {
        rejects(s, E::Expression, D::IntegerOverflow);
    }
    for s in ["0x", "0b102", "0o9", "1e", "1e+", "0xGG"] {
        rejects(s, E::Expression, D::InvalidNumber);
    }
    rejects("1e9999", E::Expression, D::FloatOverflow);
    ok("1..2", E::Expression);
    ok("1..=2", E::Expression);
    for s in ["let e\u{301} = 1;", "let 中文_变量 = 1;", "let _ = 1;"] {
        ok(s, E::Module);
    }
}
#[test]
fn recovery_preserves_following_declarations_and_insertion_locations() {
    let source = "let a = ;\nlet b = 2\nlet c = 3;\nfn next() { true }";
    let p = at(source, E::Module);
    assert!(!p.is_ok());
    assert_eq!(has(&p, K::LetDecl), 3);
    assert_eq!(has(&p, K::FnDecl), 1);
    ranges(&p, source);
    let d = p
        .diagnostics()
        .iter()
        .find(|d| d.code == D::MissingSemicolon)
        .unwrap();
    assert!(d.range.is_empty());
    assert_eq!(usize::from(d.range.start()), source.find("let c").unwrap());
    assert_eq!(d.fix.as_ref().unwrap().replacement, ";");
    let span = rejects("/* missing", E::Module, D::UnterminatedComment).diagnostics()[0].range;
    assert_eq!(byte_range(span), 0..10);
    let p = at("f\"{name", E::Expression);
    assert!(
        p.diagnostics()
            .iter()
            .any(|d| d.code == D::InvalidInterpolation)
    );
    ranges(&p, "f\"{name");
    for source in [
        "fn f(,){let b=2;} fn good(){}",
        "let a = [1, ; let b = 2;",
        "if x { broken( } fn later(){}",
    ] {
        let p = at(source, E::Module);
        assert!(!p.is_ok());
        ranges(&p, source);
    }
}
#[test]
fn every_utf8_truncation_and_seeded_arbitrary_input_terminates_losslessly() {
    let corpus = [
        "fn 名称(x = f\"{name}\") { let (a,...b) = x; if x { f() { node; }; } }",
        "/* 外 /* 内 */ */ let x = r\"\"\"多行\n文本\"\"\";",
        "class A: Base { let x; fn init(self) { self.x = 1; } }",
        "match x { {field: [a,...rest]} if a > 0 => f(...rest), _ => null, }",
    ];
    for source in corpus {
        for end in 0..=source.len() {
            if source.is_char_boundary(end) {
                let truncated = &source[..end];
                let p = at(truncated, E::Module);
                assert_eq!(p.syntax().text().to_string(), truncated);
                ranges(&p, truncated);
            }
        }
    }
    let alphabet = [
        '(', ')', '{', '}', '[', ']', '|', '&', ':', ';', ',', '=', '+', '-', '*', '/', '?', '\"',
        '\\', '\n', 'a', '0', '中', '😀',
    ];
    let mut seed = 0xa51de_u64;
    for _ in 0..500 {
        let mut source = String::new();
        for _ in 0..80 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            source.push(alphabet[(seed as usize) % alphabet.len()]);
        }
        for entry in [E::Module, E::Block, E::Expression, E::Pattern, E::Type] {
            let p = at(&source, entry);
            assert_eq!(p.syntax().text().to_string(), source);
            ranges(&p, &source);
        }
    }
}
#[test]
fn budgets_cover_recursion_flat_chains_lexing_diagnostics_and_source_admission() {
    for source in [
        format!("{}0{}", "(".repeat(5000), ")".repeat(5000)),
        format!("{}0", "!".repeat(5000)),
        format!("a{}", " + a".repeat(5000)),
        format!("{}0{}", "[".repeat(5000), "]".repeat(5000)),
        format!("{}0{}", "f\"{".repeat(1000), "}\"".repeat(1000)),
    ] {
        let p = at(&source, E::Expression);
        assert!(p.exhausted(), "{:?}", p.diagnostics());
        assert_eq!(p.syntax().text().to_string(), source);
        ranges(&p, &source);
        drop(p);
    }
    for limits in [
        Limits {
            max_tokens: 2,
            ..Default::default()
        },
        Limits {
            max_diagnostics: 2,
            ..Default::default()
        },
        Limits {
            max_depth: 4,
            ..Default::default()
        },
    ] {
        let source = "@ @ @ @ let x = (((1)));";
        let p = parse_with_options(
            source,
            ParseOptions {
                limits,
                entry: E::Module,
            },
        )
        .unwrap();
        assert!(p.exhausted());
        assert_eq!(p.syntax().text().to_string(), source);
        ranges(&p, source);
        assert!(p.diagnostics().len() <= limits.max_diagnostics);
    }
    let err = parse_with_options(
        "中文",
        ParseOptions {
            limits: Limits {
                max_source_bytes: 5,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(err.actual_bytes, 6);
    let l = lex("let x = 1; // comment\n").unwrap();
    let mut offset = 0;
    for t in l.tokens {
        assert_eq!(usize::from(t.range.start()), offset);
        offset = usize::from(t.range.end());
    }
    assert_eq!(offset, 22);
}

#[test]
fn approved_types_patterns_yield_chains_classes_and_arm_blocks_have_precise_trees() {
    let p = ok("value is (A|null) | value is B", E::Pattern);
    let outer = node(&p, K::OrPattern);
    assert_eq!(
        outer.children().map(|n| n.kind()).collect::<Vec<_>>(),
        vec![K::TypePattern, K::TypePattern]
    );
    assert_eq!(has(&p, K::UnionType), 1);
    assert_eq!(has(&p, K::GroupType), 1);
    let p = ok("fn(A) -> fn(B) -> C|null", E::Type);
    assert_eq!(has(&p, K::FunctionType), 2);
    assert_eq!(has(&p, K::UnionType), 1);
    let p = ok("(fn(A) -> R)|null", E::Type);
    assert_eq!(p.syntax().children().next().unwrap().kind(), K::UnionType);
    let p = ok("fn(A) -> R|null", E::Type);
    assert_eq!(
        p.syntax().children().next().unwrap().kind(),
        K::FunctionType
    );
    for (s, k) in [
        ("(A)", K::GroupType),
        ("(A,)", K::TupleType),
        ("(A,B)", K::TupleType),
        ("()", K::TupleType),
    ] {
        let p = ok(s, E::Type);
        assert_eq!(p.syntax().children().next().unwrap().kind(), k);
    }
    let p = ok("|x: (A|null)| x", E::Expression);
    assert_eq!(has(&p, K::UnionType), 1);
    assert_eq!(has(&p, K::ClosureExpr), 1);
    let p = ok("|x: A|null| body", E::Expression);
    assert_eq!(has(&p, K::UnionType), 0);
    assert_eq!(has(&p, K::BinaryExpr), 1); // first outer pipe always closes parameters
    let p = ok("|x: A, y: (B|null)| y", E::Expression);
    assert_eq!(has(&p, K::Param), 2);
    let source = "let incoming = yield outgoing + 1 .. end; yield;";
    let p = ok(source, E::Block);
    let yielded = node(&p, K::YieldExpr);
    assert_eq!(yielded.children().next().unwrap().kind(), K::RangeExpr);
    let p = rejects("yield a = b;", E::Block, D::InvalidAssignment);
    assert!(
        p.diagnostics()
            .iter()
            .any(|d| d.code == D::InvalidAssignment && byte_range(d.range) == (8..9))
    );
    let p = ok("a = obj[index()] = value();", E::Block);
    let outer = node(&p, K::AssignExpr);
    assert_eq!(outer.children().last().unwrap().kind(), K::AssignExpr);
    assert_eq!(has(&p, K::CallExpr), 2);
    for source in ["let x = (a = b);", "f(a = b)", "1 + (a = b)"] {
        let entry = if source.starts_with("let") {
            E::Module
        } else {
            E::Expression
        };
        let p = rejects(source, entry, D::InvalidAssignment);
        assert!(
            p.diagnostics()
                .iter()
                .any(|d| d.code == D::InvalidAssignment && &source[byte_range(d.range)] == "=")
        );
    }
    ok(
        "class Name: Parent {} let C = Name; let A = class {}; let B = class: Parent { fn init(self) {} };",
        E::Module,
    );
    rejects(
        "let C = class InternalName {};",
        E::Module,
        D::ExpectedDeclaration,
    );
    let p = ok("match x { _ => { let v = f(); v }, }", E::Expression);
    assert_eq!(has(&p, K::Block), 1);
    assert_eq!(has(&p, K::ObjectExpr), 0);
    let p = ok("match x { _ => ({name: value}), }", E::Expression);
    assert_eq!(has(&p, K::ObjectExpr), 1);
    assert_eq!(has(&p, K::Block), 0);
    let p = rejects(
        "match x { _ => {name: value}, }",
        E::Expression,
        D::MissingSemicolon,
    );
    ranges(&p, "match x { _ => {name: value}, }");
}

#[test]
fn exact_token_emission_limits_cover_eof_string_closing_and_interpolation() {
    use aside_compiler::lex_with_limits;
    for source in [
        "\"a\"",
        "\"\"",
        "f\"a{name}\"",
        "f\"{name}\"",
        "let x = 1;",
        "",
    ] {
        let needed = lex(source).unwrap().tokens.len();
        for budget in 0..=needed + 1 {
            let l = lex_with_limits(
                source,
                Limits {
                    max_tokens: budget,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                l.exhausted,
                budget < needed,
                "source={source}, budget={budget}: {l:?}"
            );
            let ordinary =
                if l.exhausted && l.tokens.last().is_some_and(|t| t.kind == K::ErrorToken) {
                    l.tokens.len() - 1
                } else {
                    l.tokens.len()
                };
            assert!(ordinary <= budget);
            assert!(l.tokens.len() <= budget + 1);
            if l.exhausted {
                assert!(l.diagnostics.iter().any(|d| d.code == D::BudgetExceeded));
            }
            let restored: String = l.tokens.iter().map(|t| t.text(source)).collect();
            assert_eq!(restored, source);
            let p = parse_with_options(
                source,
                ParseOptions {
                    entry: E::Expression,
                    limits: Limits {
                        max_tokens: budget,
                        ..Default::default()
                    },
                },
            )
            .unwrap();
            assert_eq!(p.exhausted(), l.exhausted);
            ranges(&p, source);
        }
    }
}
#[test]
fn unicode_escape_digits_are_ascii_hex_without_signs() {
    for source in [
        r#""\u{+41}""#,
        r#""\u{-41}""#,
        r#""\u{４１}""#,
        r#""\u{}""#,
        r#""\u{D800}""#,
        r#""\u{DFFF}""#,
        r#""\u{110000}""#,
    ] {
        let p = rejects(source, E::Expression, D::InvalidUnicodeEscape);
        let d = p
            .diagnostics()
            .iter()
            .find(|d| d.code == D::InvalidUnicodeEscape)
            .unwrap();
        assert_eq!(&source[byte_range(d.range)], &source[1..source.len() - 1]);
        ranges(&p, source);
    }
    for source in [
        r#""\u{0}""#,
        r#""\u{41}""#,
        r#""\u{000041}""#,
        r#""\u{D7FF}""#,
        r#""\u{E000}""#,
        r#""\u{10FFFF}""#,
    ] {
        ok(source, E::Expression);
    }
}
#[test]
fn large_named_argument_list_keeps_the_first_duplicate_range() {
    use std::fmt::Write;
    let mut source = String::from("f(");
    for i in 0..100_000 {
        write!(source, "k{i}:0,").unwrap();
    }
    let duplicate = source.len();
    source.push_str("k0:1)");
    let p = rejects(&source, E::Expression, D::DuplicateArgument);
    assert_eq!(has(&p, K::NamedArg), 100_001);
    assert!(!p.exhausted());
    let d = p
        .diagnostics()
        .iter()
        .find(|d| d.code == D::DuplicateArgument)
        .unwrap();
    assert_eq!(byte_range(d.range), duplicate..duplicate + 2);
    assert_eq!(byte_range(d.related[0].range), 2..4);
}

#[test]
fn class_heads_zero_tuples_and_closure_default_delimiters_share_the_grammar() {
    let p = ok("class: factory() { let x = 1; }", E::Expression);
    assert_eq!(has(&p, K::Children), 0);
    let p = ok("class: (factory() { node; }) { let x = 1; }", E::Expression);
    assert_eq!(has(&p, K::Children), 1);
    for (s, k) in [
        ("()", K::TuplePattern),
        ("(x)", K::GroupPattern),
        ("(x,)", K::TuplePattern),
        ("(x,y)", K::TuplePattern),
    ] {
        let p = ok(s, E::Pattern);
        assert_eq!(p.syntax().children().next().unwrap().kind(), k);
    }
    for source in [
        "|x = 1| x",
        "|x: (int|null) = null| x",
        "|x = (a | b)| x",
        "|x = {n: a | b}| x",
        "|x = f(a | b)| x",
        "|x = f\"{a | b}\"| x",
    ] {
        ok(source, E::Expression);
    }
    rejects("-true", E::Pattern, D::ExpectedPattern);
}
#[test]
fn chinese_diagnostic_renders_from_the_same_byte_labels() {
    use codespan_reporting::{
        files::SimpleFiles,
        term::{Config, emit_into_string},
    };
    let source = "let 中文 = \"\\u{+41}\";";
    let p = rejects(source, E::Module, D::InvalidUnicodeEscape);
    let mut files = SimpleFiles::new();
    let id = files.add("中文.aside", source);
    let d = p
        .diagnostics()
        .iter()
        .find(|d| d.code == D::InvalidUnicodeEscape)
        .unwrap();
    let rendered = emit_into_string(&Config::default(), &files, &d.to_codespan(id)).unwrap();
    assert!(rendered.contains("error[ASL006]"));
    assert!(rendered.contains("中文.aside:1:11"));
    assert!(rendered.contains(source));
}
