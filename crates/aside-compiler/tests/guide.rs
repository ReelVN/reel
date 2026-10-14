use aside_compiler::{EntryPoint as E, ParseOptions, SyntaxKind as K, parse_with_options};
const ZH: &str = include_str!("../../../docs/guide/zh-CN.md");
const EN: &str = include_str!("../../../docs/guide/en.md");
// One entry per author scene, rather than treating story continuations as module initialization.
const SCENES: &[(&str, E)] = &[
    ("main module", E::Module),
    ("title module", E::Module),
    ("ranges, collections and interpolation", E::Block),
    ("ordered array and table spreads", E::Block),
    ("collection editing and own fields", E::Block),
    ("dynamic stage module", E::Module),
    ("image module", E::Module),
    ("transform module", E::Module),
    ("room module", E::Module),
    ("card component", E::Module),
    ("opening module", E::Module),
    ("dialogue component", E::Module),
    ("simultaneous dialogue in story", E::Block),
    ("story registration", E::Module),
    ("entrance in story", E::Block),
    ("breathing in story", E::Block),
    ("show_cg helper", E::Module),
    ("logo startup module", E::Module),
    ("temporary hint in story", E::Block),
    ("preferences component", E::Module),
    ("picture button component", E::Module),
    ("menu style and construction", E::Module),
    ("moving group in story", E::Block),
    ("shake helper and scope in story", E::Block),
    ("mask in story", E::Block),
    ("choice component and branch in story", E::Block),
    ("save button construction", E::Module),
    ("game variables in story", E::Block),
    ("settings component", E::Module),
    ("arrive helper", E::Module),
    ("runnable button module", E::Module),
    ("modal menu module", E::Module),
    ("named input actions module", E::Module),
];
fn snippets(doc: &str) -> Vec<(usize, String)> {
    let mut result = Vec::new();
    let mut start = None;
    let mut text = String::new();
    for (i, line) in doc.lines().enumerate() {
        if line == "```aside" {
            assert!(start.is_none());
            start = Some(i + 2);
            text.clear();
        } else if line == "```" && start.is_some() {
            result.push((start.take().unwrap(), text.clone()));
        } else if start.is_some() {
            text.push_str(line);
            text.push('\n');
        }
    }
    assert!(start.is_none());
    result
}
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
                        .any(|n| n.kind() == K::Children || n.kind() == K::FnDecl),
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
