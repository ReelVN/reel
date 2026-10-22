use super::*;
use reel_ui::stage::NodeKind;

#[test]
fn inline_literals_extract_once_and_run_from_source_and_asb() {
    let code = r##"fn opening() {
        let translate = tr;
        localization.set_locale("en-US");
        if translate("你好", {en: "Hello", fr: "Bonjour"}) != "Hello" {throw "base locale";}
        if tr("保留 { braces } \\ \"\n\u{1F63A}", {en: "Keep { braces } \\ \"\n😺"}) != "Keep { braces } \\ \"\n😺" {throw "escaped literal";}
        if tr(r"raw\{braces}", {en: r"raw\{translated}"}) != r"raw\{translated}" {throw "raw string";}
        if tr("{ $n } items", {en: "{ $n ->\n [one] One\n *[other] { $n } items\n }"}, params: {n:1}) != "One" {throw "explicit numeric params";}
        localization.set_locale("zh-CN");
        if tr("原文", {["zh-CN"]: "简体", ["zh-TW"]: "繁體"}) != "简体" {throw "locale string keys";}
        if tr("literal { $unknown }", {fr: "French only"}) != "literal { $unknown }" {throw "literal fallback";}
        narrator("inline complete");
    }"##;
    let compiled = source::compile_story(code, "inline", 1, "opening").unwrap();
    assert_eq!(compiled.catalogs.catalogs.len(), 4);
    assert_eq!(compiled.catalogs.messages.len(), 6);
    for value in &compiled.program.package().constants {
        if let bc::Const::Utf8(text) = value {
            assert!(
                !text.contains("Bonjour")
                    && !text.contains("繁體")
                    && !text.contains("French only")
            );
        }
    }
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        let Some(crate::InteractionView::Dialogue(line)) = session.view().interaction else {
            panic!("dialogue")
        };
        assert_eq!(line.text(), "inline complete");
        collect(&mut vm);
        assert_eq!(vm.locale().as_deref(), Some("zh-CN"));
    }
}

#[test]
fn interpolation_evaluates_source_once_in_argument_order_and_reuses_values() {
    let code = r#"fn opening() {
        let mut order = "";
        fn value(tag) {order += tag; tag}
        fn language() {order += "L"; "en"}
        localization.set_locale("en");
        let translated = tr(f"原{value("A")}|{true}|{null}|{17}|{1.25}", {
            en: f"{1.25}/{value("A")}/{null}/{true}/{17}/{value("A")}",
            fr: f"{true}",
        }, locale: language());
        if order != "AL" {throw f"source/locale order: {order}";}
        let output = text(translated);
        localization.set_locale("ja");
        let fallback = text(tr(f"fallback {value("B")} {{braces}}", {en: f"{value("B")}"}));
        if order != "ALB" {throw "fallback duplicated source";}
        let a = "one"; let b = "two";
        let reordered = text(tr(f"{a}:{b}", {en: f"{b}:{a}:{a}"}, params: null));
        localization.set_locale("en");
        let mut count = 0;
        fn next_value() {count += 1; count}
        let repeated = text(tr(f"{next_value()}:{next_value()}", {en: f"{next_value()}/{next_value()}"}));
        if count != 2 {throw "repeated source occurrences were merged or reevaluated";}
        localization.set_locale("fr");
        let omitted = text(tr(f"{next_value()}:{next_value()}", {fr: "omitted"}));
        if count != 4 {throw "omitted translation skipped source effects";}
        stage.replace(column { output; fallback; reordered; repeated; omitted; });
        narrator("once");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        let texts = vm
            .stage()
            .nodes()
            .filter_map(|node| {
                if let NodeKind::Text(text) = &node.kind {
                    Some(text.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert!(texts.iter().any(|text| text == "\u{2068}1.25\u{2069}/\u{2068}A\u{2069}/\u{2068}null\u{2069}/\u{2068}true\u{2069}/\u{2068}17\u{2069}/\u{2068}A\u{2069}"), "{texts:?}");
        assert!(
            texts
                .iter()
                .any(|text| text == "fallback \u{2068}B\u{2069} {braces}"),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text == "\u{2068}1\u{2069}/\u{2068}2\u{2069}"),
            "{texts:?}"
        );
        assert!(texts.iter().any(|text| text == "omitted"));
        collect(&mut vm);
    }
}

#[test]
fn invalid_inline_forms_report_the_original_source_expression() {
    for (body, detail, offending) in [
        (
            r#"let value="dynamic"; tr(value, {en:"English"});"#,
            "literal",
            "value",
        ),
        (
            r#"let translations={en:"English"}; tr("source", translations);"#,
            "static locale table",
            "translations",
        ),
        (
            r#"tr("source", {["not a locale"]: "text"});"#,
            "locale",
            "not a locale",
        ),
        (
            r#"let a=1;let b=2;tr(f"{a}",{en:f"{b}"});"#,
            "source string",
            "b",
        ),
        (
            r#"fn next(){1}tr(f"{next()}/{next()}",{en:f"{next()}"});"#,
            "ambiguous",
            "next()",
        ),
        (
            r#"let a=1;tr(f"{a}",{en:f"{a}"},params:{a:1});"#,
            "already supplies",
            "{a:1}",
        ),
        (r#"let escaped=[tr];"#, "statically resolved", "tr"),
        (
            r#"let alias=tr;fn forward(value){value}forward(alias);"#,
            "statically resolved",
            "alias",
        ),
        (r#"let mut alias=tr;"#, "statically resolved", "tr"),
    ] {
        let code = format!("fn opening() {{{body}}}");
        let source::SourceError::Translation(error) =
            source::compile_story(&code, "diagnostic", 1, "opening")
                .err()
                .unwrap_or_else(|| panic!("expected rejection for {code}"))
        else {
            panic!("expected translation diagnostic for {code}")
        };
        assert!(error.message.contains(detail), "{error:?}");
        let bytes = usize::from(error.span.start())..usize::from(error.span.end());
        assert!(code[bytes].contains(offending), "{error:?}: {code}");
        assert_eq!(error.module, "diagnostic");
    }
}

#[test]
fn catalog_count_does_not_change_runtime_argument_construction() {
    let one = source::compile_story(
        r#"fn opening(){tr("source",{en:"one"});}"#,
        "cost",
        1,
        "opening",
    )
    .unwrap();
    let many = source::compile_story(r#"fn opening(){tr("source",{en:"one",fr:"two",de:"three",ja:"four",ko:"five",it:"six",es:"seven",pt:"eight"});}"#, "cost", 1, "opening").unwrap();
    assert_eq!(
        one.program.package().functions[one.entry.index()]
            .code
            .len(),
        many.program.package().functions[many.entry.index()]
            .code
            .len()
    );
    assert_eq!(
        one.program.package().functions[one.entry.index()].register_count,
        many.program.package().functions[many.entry.index()].register_count
    );
    assert!(
        !many
            .program
            .package()
            .functions
            .iter()
            .flat_map(|function| &function.code)
            .any(|instruction| instruction.opcode == bc::Opcode::MakeTable as u16)
    );
    assert_eq!(many.catalogs.catalogs.len(), 8);
    eprintln!(
        "inline runtime shape: locales=1/8, entry instructions={}, registers={}, runtime locale tables=0",
        many.program.package().functions[many.entry.index()]
            .code
            .len(),
        many.program.package().functions[many.entry.index()].register_count
    );
}

#[test]
fn editor_validation_preserves_previews_and_regions_after_semantic_errors() {
    use aside_compiler::{AnalysisOptions, editor, semantic::MemoryResolver};
    let code = r#"fn opening(){let player="Ada";tr(f"Hello {player}",{en:f"Hi {player}",fr:f"Bonjour {missing}"});}"#;
    let mut resolver = MemoryResolver::default();
    for module in &source::api().modules {
        resolver.insert(module.clone());
    }
    let parsed = aside_compiler::parse(code).unwrap();
    let recovered = editor::analyze(
        &parsed,
        &AnalysisOptions {
            hosts: source::api().globals.clone(),
            ..Default::default()
        },
        &mut resolver,
    );
    assert!(!recovered.diagnostics.is_empty());
    let result = source::analyze_translations(recovered.module().unwrap());
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0].source.as_deref(), Some("Hello {player}"));
    assert_eq!(
        result.calls[0].variants[1].text.as_deref(),
        Some("Bonjour {missing}")
    );
    assert_eq!(result.calls[0].locales, ["en", "fr"]);
    assert!(result.calls[0].table_span.is_some());
    assert_eq!(result.calls[0].holes.len(), 3);
    assert_eq!(result.calls[0].holes[1].source_slot, Some(0));
    assert_eq!(result.calls[0].holes[2].source_slot, None);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("source string"))
    );
}

#[test]
fn compile_extensions_require_exact_identity_and_remain_inside_lowering_limits() {
    let compiled = source::compile_module(
        r#"fn opening(){tr("source",{en:"translation"});}"#,
        "extension",
        "extension.aside",
        1,
    )
    .unwrap();
    let (_, original) =
        source::extract_translations(std::slice::from_ref(&compiled.module)).unwrap();
    let run = |plans: &[Vec<aside_compiler::lowering::CallExpansion>], max_work| {
        aside_host::lower(
            &compiled.module,
            source::contracts(),
            &source::api().functions,
            source::types(),
            &registry::capabilities(),
            aside_host::CompileOptions {
                lowering: aside_compiler::lowering::Options {
                    limits: aside_compiler::lowering::Limits {
                        max_work,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                expansions: plans,
            },
        )
    };
    let mut changed = original.clone();
    changed[0][0].reference.export = Some("narrator".into());
    let aside_host::BuildError::Lowering(error) = run(&changed, u64::MAX).unwrap_err() else {
        panic!("link error")
    };
    assert_eq!(
        error.kind,
        aside_compiler::lowering::ErrorKind::LinkContract
    );
    assert_eq!(
        error.span,
        compiled.module.hir().node(original[0][0].call).span
    );
    let mut changed = original.clone();
    let duplicate = changed[0][0].clone();
    changed[0].push(duplicate);
    let aside_host::BuildError::Lowering(error) = run(&changed, u64::MAX).unwrap_err() else {
        panic!("duplicate error")
    };
    assert_eq!(
        error.kind,
        aside_compiler::lowering::ErrorKind::LinkContract
    );
    let full = run(&original, u64::MAX).unwrap();
    let aside_host::BuildError::Lowering(error) =
        run(&original, full.stats.work_units - 1).unwrap_err()
    else {
        panic!("budget error")
    };
    assert_eq!(error.kind, aside_compiler::lowering::ErrorKind::Limit);
    assert!(error.stats.work_units < full.stats.work_units);
}

#[test]
fn identical_content_packages_share_keys_across_modules() {
    use aside_compiler::semantic::SourceModule;
    let project = source::compile_project(
        &[
            SourceModule {
                canonical_path: "a".into(),
                source_path: Some("a.aside".into()),
                version: "1".into(),
                source: "pub fn a(){tr(\"source\",{en:\"translated\"})}".into(),
            },
            SourceModule {
                canonical_path: "b".into(),
                source_path: Some("b.aside".into()),
                version: "1".into(),
                source: "pub fn b(){tr(\"source\",{en:\"translated\"})}".into(),
            },
        ],
        &Default::default(),
        1,
    )
    .unwrap();
    assert_eq!(project.catalogs.messages.len(), 2);
    assert_eq!(
        project.catalogs.messages[0].key,
        project.catalogs.messages[1].key
    );
    assert_eq!(project.catalogs.catalogs["en"].lines().count(), 1);
}
