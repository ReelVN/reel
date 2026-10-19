use aside_compiler::{semantic::*, *};
const ZH: &str = include_str!("../../../docs/guide/zh-CN.md");
const EN: &str = include_str!("../../../docs/guide/en.md");
#[path = "support/guide_scenes.rs"]
mod scenes;
use scenes::{SCENES, snippets};
// These names are the documented reel::prelude symbols, not names harvested
// from unresolved diagnostics. Their dynamic members remain runtime contracts.
const PRELUDE: &[&str] = &[
    "audio",
    "rich",
    "camera",
    "view",
    "window",
    "particles",
    "emit",
    "pause",
    "resume",
    "stop",
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
    "text_input",
    "delay",
    "binding",
    "image",
    "video",
    "slider",
    "toggle",
    "checkbox",
    "radio",
    "switcher",
    "page",
    "list",
    "grid",
    "scroll",
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
    "scope",
    "mask",
    "bind",
    "unbind",
    "derive",
    "state",
    "persistent",
    "model",
    "set",
    "localization",
    "tr",
];
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
    let mut errors = Vec::new();
    for (i, ((_, chinese), (_, english))) in zh.iter().zip(&en).enumerate() {
        let (name, entry) = SCENES[i];
        for (edition, source) in [("zh-CN", chinese), ("en", english)] {
            let block = entry == EntryPoint::Block;
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
