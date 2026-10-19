use aside_compiler::{EntryPoint as E, ParseOptions, SyntaxKind as K, parse_with_options};
const ZH: &str = include_str!("../../../docs/guide/zh-CN.md");
const EN: &str = include_str!("../../../docs/guide/en.md");
// One entry per author scene, rather than treating story continuations as module initialization.
const SCENES: &[(&str, E)] = &[
    ("main module", E::Module),
    ("title module", E::Module),
    ("ranges, collections and interpolation", E::Block),
    ("table lookup defaults", E::Block),
    ("stable array sorting", E::Block),
    ("ordered array and table spreads", E::Block),
    ("tuple bindings", E::Block),
    ("value match expressions", E::Block),
    ("collection editing and own fields", E::Block),
    ("intrinsic panel layout", E::Block),
    ("typed author classes", E::Module),
    ("presentation wait", E::Block),
    ("locally composed control", E::Block),
    ("layer ordering", E::Block),
    ("dynamic stage module", E::Module),
    ("image module", E::Module),
    ("transform module", E::Module),
    ("camera framing", E::Block),
    ("room module", E::Module),
    ("card component", E::Module),
    ("selective history", E::Block),
    ("opening module", E::Module),
    ("dialogue component", E::Module),
    ("simultaneous dialogue in story", E::Block),
    ("story registration", E::Module),
    ("entrance in story", E::Block),
    ("parallel entrance", E::Block),
    ("breathing in story", E::Block),
    ("timed delay in story", E::Block),
    ("show_cg helper", E::Module),
    ("video startup", E::Block),
    ("temporary hint in story", E::Block),
    ("preferences component", E::Module),
    ("picture button component", E::Module),
    ("interaction state callback", E::Block),
    ("menu style and construction", E::Module),
    ("moving group in story", E::Block),
    ("scoped choreography in story", E::Block),
    ("mask in story", E::Block),
    ("subtree inversion", E::Block),
    ("subtree blending", E::Block),
    ("authored line wrapping", E::Block),
    ("component result in story", E::Block),
    ("choice component and branch in story", E::Block),
    ("clear dialogue presentation", E::Block),
    ("retain recent history", E::Block),
    ("save button construction", E::Module),
    ("clear read progress", E::Block),
    ("shared model controls", E::Block),
    ("game variables in story", E::Block),
    ("persistent settings", E::Block),
    ("settings component", E::Module),
    ("arrive helper", E::Module),
    ("bilingual dialogue", E::Block),
    ("external translations in story", E::Block),
    ("runnable button module", E::Module),
    ("modal menu module", E::Module),
    ("named input actions module", E::Module),
    ("shared selection controls", E::Block),
    ("reading settings module", E::Module),
    ("editable text in story", E::Block),
    ("shared property module", E::Module),
    ("author audio", E::Module),
    ("audio continuation", E::Block),
    ("queued audio continuation", E::Block),
    ("window configuration", E::Block),
    ("continuous particles", E::Block),
    ("burst particles", E::Block),
    ("accepted node geometry", E::Block),
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
