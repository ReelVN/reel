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
                let mut callbacks = root
                    .descendants()
                    .filter(|n| {
                        n.kind() == K::NamedArg
                            && n.text().to_string().trim_start().starts_with("on_click:")
                    })
                    .peekable();
                assert!(
                    callbacks.peek().is_some(),
                    "on_click argument missing in {name}"
                );
                assert!(
                    callbacks.all(|arg| arg
                        .children()
                        .any(|n| matches!(n.kind(), K::NameExpr | K::ClosureExpr))),
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

mod semantic_guides {
    use aside_compiler::{semantic::*, *};
    const ZH: &str = include_str!("../../../docs/guide/zh-CN.md");
    const EN: &str = include_str!("../../../docs/guide/en.md");
    use super::scenes;
    use scenes::{SCENES, snippets};
    // These names are the documented reel::prelude symbols, not names harvested
    // from unresolved diagnostics. Their dynamic members remain runtime contracts.
    const PRELUDE: &[&str] = &[
        "build",
        "audio",
        "assets",
        "appearance",
        "effect",
        "texture",
        "rich",
        "camera",
        "clock",
        "cursor",
        "view",
        "window",
        "particles",
        "emit",
        "pause",
        "resume",
        "stop",
        "stage",
        "game",
        "saves",
        "column",
        "row",
        "grid",
        "stack",
        "text",
        "button",
        "input",
        "text_input",
        "text_area",
        "popup",
        "delay",
        "binding",
        "image",
        "image_skin",
        "layered_image",
        "video",
        "slider",
        "toggle",
        "checkbox",
        "radio",
        "list",
        "scroll",
        "scrollbar",
        "page_stack",
        "tab_bar",
        "character",
        "narrator",
        "say",
        "choose",
        "completion",
        "parallel",
        "sequence",
        "repeat",
        "tween",
        "run",
        "run_flow",
        "scope",
        "reading",
        "mask",
        "bind",
        "unbind",
        "derive",
        "persistent",
        "model",
        "set",
        "localization",
        "tr",
    ];
    fn prelude_symbol(name: &str) -> ExternalSymbol {
        ExternalSymbol {
            must_use_result: matches!(
                name,
                "text"
                    | "text_input"
                    | "text_area"
                    | "popup"
                    | "image"
                    | "video"
                    | "particles"
                    | "row"
                    | "grid"
                    | "column"
                    | "stack"
                    | "cursor"
                    | "button"
                    | "input"
                    | "slider"
                    | "toggle"
                    | "checkbox"
                    | "radio"
                    | "scroll"
                    | "scrollbar"
                    | "page_stack"
                    | "tab_bar"
                    | "camera"
                    | "list"
                    | "view"
                    | "layered_image"
            ),
            ..ExternalSymbol::dynamic(name)
        }
    }
    #[test]
    fn all_bilingual_scenes_analyze_with_explicit_host_and_lexical_context() {
        let zh = snippets(ZH);
        let en = snippets(EN);
        assert_eq!(zh.len(), SCENES.len());
        assert_eq!(
            zh.iter().map(|(_, code)| code).collect::<Vec<_>>(),
            en.iter().map(|(_, code)| code).collect::<Vec<_>>()
        );
        let mut resolver = MemoryResolver::default();
        resolver.aliases.extend([
            ("ui".into(), "game::ui".into()),
            ("story".into(), "game::story".into()),
        ]);
        resolver.insert(ModuleInterface {
            canonical_path: "reel::prelude".into(),
            version: "guide-v1".into(),
            exports: PRELUDE.iter().map(|&n| prelude_symbol(n)).collect(),
        });
        for (path, name) in [
            ("std::math", "curve"),
            ("std::number", "format"),
            ("game::ui::title", "title"),
            ("game::story::opening", "opening"),
            ("game::ui::dialogue", "dialogue_view"),
            ("game::ui::room", "room_view"),
        ] {
            resolver.insert(ModuleInterface {
                canonical_path: path.into(),
                version: "guide-v1".into(),
                exports: vec![ExternalSymbol::dynamic(name)],
            });
        }
        resolver.insert(ModuleInterface {
            canonical_path: "std::math".into(),
            version: "guide-v1".into(),
            exports: ["curve", "to_int"]
                .into_iter()
                .map(ExternalSymbol::dynamic)
                .collect(),
        });
        resolver.insert(ModuleInterface {
            canonical_path: "reel::types".into(),
            version: "guide-v1".into(),
            exports: ["Fragment", "Texture"]
                .into_iter()
                .map(|name| ExternalSymbol {
                    type_only: true,
                    ty: TypeHint::Named(format!("reel::types::{name}")),
                    ..ExternalSymbol::dynamic(name)
                })
                .collect(),
        });
        resolver.insert(ModuleInterface {
            canonical_path: "std::resume".into(),
            version: "guide-v1".into(),
            exports: ["state", "root", "flow", "point"]
                .into_iter()
                .map(ExternalSymbol::dynamic)
                .collect(),
        });
        let mut errors = Vec::new();
        for (i, ((_, chinese), (_, english))) in zh.iter().zip(&en).enumerate() {
            let (name, entry) = SCENES[i];
            for (edition, source) in [("zh-CN", chinese), ("en", english)] {
                let block = entry == EntryPoint::Block;
                let options = AnalysisOptions {
                    module: format!("guide::{edition}::{i}"),
                    version: "guide-v1".into(),
                    context: if block {
                        AnalysisContext::Function
                    } else {
                        AnalysisContext::Module
                    },
                    // Only implemented Reel prelude names are admitted.
                    // Proposed interfaces live in parser-only fixtures, not a fake host environment.
                    hosts: PRELUDE.iter().map(|&name| prelude_symbol(name)).collect(),
                    enclosing: scenes::enclosing(name)
                        .iter()
                        .map(|&binding| EnclosingBinding {
                            symbol: ExternalSymbol::dynamic(binding),
                            mutable: scenes::mutable(name, binding),
                        })
                        .collect(),
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
                if !result.is_ok() {
                    errors.push(format!(
                        "{edition} scene {i} {name}: {:?}",
                        result.diagnostics
                    ));
                    continue;
                }
                if name == "scoped choreography in story" {
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
        assert!(errors.is_empty(), "{}", errors.join("\n"));
    }
}
