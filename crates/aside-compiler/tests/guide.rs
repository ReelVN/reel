use aside_compiler::{ParseOptions, SyntaxKind as K, parse_with_options};
const ZH: &str = include_str!("../../../docs/guide/zh-CN.md");
const EN: &str = include_str!("../../../docs/guide/en.md");
// One entry per author scene, rather than treating story continuations as module initialization.
#[path = "support/guide_scenes.rs"]
mod scenes;
use scenes::{SCENES, snippets};
#[test]
fn every_bilingual_author_scene_parses_in_its_actual_context() {
    let zh = snippets(ZH);
    let en = snippets(EN);
    assert_eq!(zh.len(), SCENES.len());
    assert_eq!(en.len(), SCENES.len());
    for (i, ((zh_line, source), (en_line, english))) in zh.iter().zip(&en).enumerate() {
        assert_eq!(
            source, english,
            "guide editions diverged in scene {}",
            SCENES[i].0
        );
        let (name, entry) = SCENES[i];
        for (edition, line, code) in [("zh-CN", zh_line, source), ("en", en_line, english)] {
            let parsed = parse_with_options(
                code,
                ParseOptions {
                    entry,
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(
                parsed.is_ok(),
                "scene {i} {name}, docs/guide/{edition}.md:{line}: {:?}",
                parsed.diagnostics()
            );
            let root = parsed.syntax();
            assert_eq!(root.text().to_string(), *code);
            if source.contains("on_click:") {
                assert!(
                    root.descendants().any(|n| n.kind() == K::ClosureExpr),
                    "callback missing in {name}"
                );
            }
            if source.contains("await ") {
                assert!(
                    root.descendants().any(|n| n.kind() == K::PrefixExpr),
                    "await missing in {name}"
                );
            }
            if source.contains("() {") || source.contains("gap: 12) {") {
                assert!(
                    root.descendants()
                        .any(|n| matches!(n.kind(), K::Children | K::FnDecl | K::ForStmt)),
                    "body missing in {name}"
                );
            }
        }
    }
}

#[test]
fn every_guide_prefix_recovers_without_losing_utf8_bytes() {
    for (i, (line, source)) in snippets(ZH).iter().enumerate() {
        for end in 0..=source.len() {
            if !source.is_char_boundary(end) {
                continue;
            }
            let prefix = &source[..end];
            let parsed = parse_with_options(
                prefix,
                ParseOptions {
                    entry: SCENES[i].1,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                parsed.syntax().text().to_string(),
                prefix,
                "{} at line {line}, byte {end}",
                SCENES[i].0
            );
            for d in parsed.diagnostics() {
                assert!(usize::from(d.range.end()) <= end);
                assert!(
                    prefix.is_char_boundary(usize::from(d.range.start()))
                        && prefix.is_char_boundary(usize::from(d.range.end()))
                );
            }
        }
    }
}

#[test]
fn preserved_planned_examples_keep_syntax_and_prefix_recovery_coverage() {
    for &(name, source, entry) in scenes::PLANNED_SOURCES {
        let parsed = parse_with_options(
            source,
            ParseOptions {
                entry,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(parsed.is_ok(), "{name}: {:?}", parsed.diagnostics());
        assert_eq!(parsed.syntax().text().to_string(), source);
        for end in 0..=source.len() {
            if !source.is_char_boundary(end) {
                continue;
            }
            let prefix = &source[..end];
            let parsed = parse_with_options(
                prefix,
                ParseOptions {
                    entry,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                parsed.syntax().text().to_string(),
                prefix,
                "{name} at byte {end}"
            );
            for diagnostic in parsed.diagnostics() {
                assert!(usize::from(diagnostic.range.end()) <= end);
                assert!(
                    prefix.is_char_boundary(usize::from(diagnostic.range.start()))
                        && prefix.is_char_boundary(usize::from(diagnostic.range.end()))
                );
            }
        }
    }
}
