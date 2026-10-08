use aside_compiler::{semantic::*, *};
const ZH: &str = include_str!("../../../docs/guide/zh-CN.md");
const EN: &str = include_str!("../../../docs/guide/en.md");
const BLOCKS: &[usize] = &[6, 8, 9, 12, 16, 17, 18, 19, 21];
// These names are the documented reel::prelude symbols, not names harvested
// from unresolved diagnostics. Their dynamic members remain runtime contracts.
const PRELUDE: &[&str] = &[
    "stage",
    "game",
    "story",
    "settings",
    "saves",
    "column",
    "row",
    "stack",
    "group",
    "text",
    "button",
    "image",
    "video",
    "slider",
    "switcher",
    "page",
    "list",
    "grid",
    "scroll",
    "character",
    "narrator",
    "say",
    "choose",
    "parallel",
    "sequence",
    "repeat",
    "tween",
    "run",
    "scope",
    "mask",
    "bind",
    "derive",
    "state",
    "persistent",
    "model",
    "set",
];
// Actual surrounding author scope described in the prose for each fragment.
const CONTEXT: &[(usize, &[&str])] = &[
    (3, &["column"]),
    (4, &["show_cg"]),
    (5, &["column", "text", "bind", "derive"]),
    (6, &["mio", "character", "say"]),
    (7, &["story"]),
    (8, &["view", "parallel", "tween", "run"]),
    (9, &["view", "mio", "repeat", "sequence", "tween", "run"]),
    (10, &["scope", "image", "game"]),
    (12, &["view", "mio", "photo_panel", "button"]),
    (
        13,
        &[
            "switcher", "page", "text", "slider", "bind", "settings", "image", "column", "row",
            "button",
        ],
    ),
    (14, &["button", "image", "derive"]),
    (15, &["button", "image", "text", "game", "opening"]),
    (
        16,
        &[
            "group",
            "image",
            "dialogue_view",
            "game",
            "stage",
            "run",
            "tween",
        ],
    ),
    (17, &["view", "repeat", "sequence", "tween", "scope"]),
    (18, &["view", "mask", "group", "image", "run", "tween"]),
    (
        19,
        &[
            "list",
            "bind",
            "button",
            "text",
            "choose",
            "by_window",
            "by_door",
        ],
    ),
    (20, &["saves", "button"]),
    (21, &["state", "persistent", "set"]),
    (22, &["column", "button"]),
    (23, &["parallel", "tween"]),
];
fn snippets(doc: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = None;
    for line in doc.lines() {
        if line == "```aside" {
            current = Some(String::new());
        } else if line == "```" {
            if let Some(s) = current.take() {
                result.push(s);
            }
        } else if let Some(s) = &mut current {
            s.push_str(line);
            s.push('\n');
        }
    }
    result
}
#[test]
fn all_24_bilingual_scenes_analyze_with_explicit_host_and_lexical_context() {
    let zh = snippets(ZH);
    let en = snippets(EN);
    assert_eq!(zh.len(), 24);
    assert_eq!(zh, en);
    let mut resolver = MemoryResolver::default();
    resolver.insert(ModuleInterface {
        canonical_path: "reel::prelude".into(),
        version: "guide-1.1".into(),
        exports: PRELUDE
            .iter()
            .map(|&n| ExternalSymbol::dynamic(n))
            .collect(),
    });
    for (path, name) in [
        ("game::ui::title", "title"),
        ("game::story::opening", "opening"),
        ("game::ui::dialogue", "dialogue_view"),
        ("game::ui::room", "room_view"),
    ] {
        resolver.insert(ModuleInterface {
            canonical_path: path.into(),
            version: "guide-1.1".into(),
            exports: vec![ExternalSymbol::dynamic(name)],
        });
    }
    for (i, source) in zh.iter().enumerate() {
        let block = BLOCKS.contains(&i);
        let options = AnalysisOptions {
            module: format!("guide::{i}"),
            context: if block {
                AnalysisContext::Function
            } else {
                AnalysisContext::Module
            },
            hosts: CONTEXT
                .iter()
                .find(|(scene, _)| *scene == i)
                .map(|(_, names)| names.iter().map(|&n| ExternalSymbol::dynamic(n)).collect())
                .unwrap_or_default(),
            ..Default::default()
        };
        let parsed = parse_with_options(
            source,
            ParseOptions {
                entry: if block {
                    EntryPoint::Block
                } else {
                    EntryPoint::Module
                },
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            parsed.is_ok(),
            "scene {i} parse: {:?}",
            parsed.diagnostics()
        );
        let result = analyze(&parsed, &options, &mut resolver);
        assert!(
            result.is_ok(),
            "scene {i} analysis: {:?}",
            result.diagnostics
        );
    }
}
