use aside_compiler::{semantic::*, *};
const ZH: &str = include_str!("../../../docs/guide/zh-CN.md");
const EN: &str = include_str!("../../../docs/guide/en.md");
const BLOCKS: &[usize] = &[2, 3, 4, 12, 14, 15, 18, 22, 23, 24, 25, 27, 28];
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
    "input",
    "binding",
    "image",
    "video",
    "slider",
    "toggle",
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
    "unbind",
    "derive",
    "state",
    "persistent",
    "model",
    "set",
];
// Actual surrounding author scope described in the prose for each fragment.
const CONTEXT: &[(usize, &[&str])] = &[
    (2, &["text"]),
    (3, &["text"]),
    (9, &["column"]),
    (10, &["show_cg"]),
    (11, &["column", "text", "bind", "derive"]),
    (12, &["mio", "character", "say"]),
    (13, &["story"]),
    (14, &["view", "parallel", "tween", "run"]),
    (15, &["view", "mio", "repeat", "sequence", "tween", "run"]),
    (16, &["scope", "image", "game"]),
    (18, &["view", "mio", "photo_panel", "button"]),
    (
        19,
        &[
            "switcher", "page", "text", "slider", "bind", "settings", "image", "column", "row",
            "button",
        ],
    ),
    (20, &["button", "image", "derive"]),
    (21, &["button", "image", "text", "game", "opening"]),
    (
        22,
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
    (23, &["view", "repeat", "sequence", "tween", "scope"]),
    (24, &["view", "mask", "group", "image", "run", "tween"]),
    (
        25,
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
    (26, &["saves", "button"]),
    (27, &["model", "slider", "toggle", "text", "bind"]),
    (28, &["state", "persistent", "set"]),
    (29, &["column", "button"]),
    (30, &["parallel", "tween"]),
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
fn all_bilingual_scenes_analyze_with_explicit_host_and_lexical_context() {
    let zh = snippets(ZH);
    let en = snippets(EN);
    assert_eq!(zh.len(), 36);
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
    for (i, (chinese, english)) in zh.iter().zip(&en).enumerate() {
        for (edition, source) in [("zh-CN", chinese), ("en", english)] {
            let block = BLOCKS.contains(&i);
            let options = AnalysisOptions {
                module: format!("guide::{edition}::{i}"),
                version: "guide-design-1.1".into(),
                context: if block {
                    AnalysisContext::Function
                } else {
                    AnalysisContext::Module
                },
                // The documented Reel prelude supplies the outer host scope.
                hosts: PRELUDE
                    .iter()
                    .map(|&name| ExternalSymbol::dynamic(name))
                    .collect(),
                enclosing: CONTEXT
                    .iter()
                    .find(|(scene, _)| *scene == i)
                    .map(|(_, names)| {
                        names
                            .iter()
                            .filter(|n| !PRELUDE.contains(n))
                            .map(|&n| EnclosingBinding {
                                symbol: ExternalSymbol::dynamic(n),
                                mutable: false,
                            })
                            .collect()
                    })
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
            if i == 23 {
                let module = result.module().unwrap();
                assert!(
                    module
                        .functions()
                        .iter()
                        .flat_map(|f| &f.captures)
                        .any(|c| module
                            .hir()
                            .name(module.symbols()[c.symbol.0 as usize].name)
                            == "view")
                );
            }
        }
    }
}
