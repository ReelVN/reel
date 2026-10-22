use aside_compiler::{editor::EditorModule, hir::*, semantic::*};

fn module(source: &str) -> aside_compiler::editor::EditorAnalysis {
    let parsed = aside_compiler::parse(source).unwrap();
    let analysis =
        aside_compiler::editor::analyze(&parsed, &AnalysisOptions::default(), &mut NoModules);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis
}
fn expression<'a>(
    analysis: &'a aside_compiler::editor::EditorAnalysis,
    text: &str,
) -> &'a SourceType {
    expression_in(analysis.module().unwrap(), text)
}
fn expression_in<'a>(module: EditorModule<'a>, text: &str) -> &'a SourceType {
    let (index, _) = module
        .hir()
        .nodes
        .iter()
        .enumerate()
        .rev()
        .find(|(_, node)| module.hir().text(node.span) == text)
        .unwrap_or_else(|| panic!("missing {text}"));
    module
        .source_types()
        .value(NodeId(index as u32))
        .unwrap_or_else(|| panic!("untyped {text}"))
        .as_ref()
}

#[test]
fn ordinary_returns_parts_and_arrays_preserve_source_fields() {
    let module = module(
        r#"
fn number(flag: bool) { if flag { return 7; } 11 }
fn parts() -> table { {root: {label: "Start"}, captions: ["one", "two"]} }
fn main() { let ui=parts(); let n=number(true); ui.root.label; ui.captions[0]; n }
"#,
    );
    assert_eq!(expression(&module, "ui.root.label").ty, TypeHint::String);
    assert_eq!(expression(&module, "ui.captions[0]").ty, TypeHint::String);
    assert_eq!(expression(&module, "number(true)").ty, TypeHint::Int);
    let module = module.module().unwrap();
    let (index, _) = module
        .hir()
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| module.hir().text(node.span) == "ui.root.label")
        .unwrap();
    let definition = module
        .source_types()
        .definition(NodeId(index as u32))
        .unwrap();
    assert_eq!(module.hir().text(definition.range), "label");
    let function = module
        .functions()
        .iter()
        .find(|f| {
            module
                .hir()
                .text(module.hir().node(f.node).span)
                .starts_with("fn parts")
        })
        .unwrap();
    assert_eq!(module.types()[function.return_type.0 as usize], Type::Table);
}

#[test]
fn alias_writes_and_dynamic_keys_do_not_keep_stale_field_types() {
    let module = module(
        r#"
fn use_dynamic(value: table) {}
fn main(key: string) {
    let parts={label:"old", count:1}; let alias=parts;
    alias.label=12; let updated=parts.label;
    parts[key]=false; let dynamic=parts.count;
    let next={name:"next"}; use_dynamic(next); let escaped=next.name;
    (updated,dynamic,escaped)
}
"#,
    );
    assert_eq!(expression(&module, "parts.label").ty, TypeHint::Int);
    assert_eq!(expression(&module, "parts.count").ty, TypeHint::Any);
    assert_eq!(expression(&module, "next.name").ty, TypeHint::Any);
}

#[test]
fn imported_factory_propagates_fields_without_changing_runtime_contracts() {
    let source = |path: &str, source: &str| aside_compiler::editor::EditorSource {
        source: SourceModule {
            canonical_path: path.into(),
            source_path: None,
            version: "v1".into(),
            source: source.into(),
        },
        parsed: aside_compiler::parse(source).unwrap(),
    };
    let analysis = aside_compiler::editor::analyze_project(
        &[
            source(
                "main",
                r#"use ui::parts; fn main(){let view=parts();view.label;view.names[0]}"#,
            ),
            source(
                "ui",
                r#"pub fn parts()->table{{label:"Ready",names:["first"]}}"#,
            ),
        ],
        &AnalysisOptions::default(),
        &mut NoModules,
    )
    .unwrap();
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let modules: Vec<_> = analysis.modules().collect();
    let main = modules
        .iter()
        .find(|module| module.canonical_path() == "main")
        .unwrap();
    assert_eq!(expression_in(*main, "view.label").ty, TypeHint::String);
    assert_eq!(expression_in(*main, "view.names[0]").ty, TypeHint::String);
    let (index, _) = main
        .hir()
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| main.hir().text(node.span) == "view.label")
        .unwrap();
    let definition = main
        .source_types()
        .definition(NodeId(index as u32))
        .unwrap();
    assert_eq!(definition.module.as_ref(), "ui");
    let ui = modules
        .iter()
        .find(|module| module.canonical_path() == "ui")
        .unwrap();
    assert_eq!(ui.hir().text(definition.range), "label");
}

#[test]
fn container_access_preserves_shallow_aliases_and_compound_operation_types() {
    let module = module(
        r#"
fn parts(){let child={name:"old"};{child:child,rows:[child]}}
fn main(){
 let first={child:{name:"old"}};let alias=first["child"];alias.name=12;let indexed=first.child.name;
 let child={name:"old"};let entries=[child];entries[0].name=12;let array=child.name;
 let original={child:{name:"old"}};let copy={...original};copy.child.name=12;let spread=original.child.name;
 let view=parts();view.rows[0].name=12;let returned=view.child.name;
 let number={value:1.5};number.value+=1;let compound=number.value;
 (indexed,array,spread,returned,compound)
}
"#,
    );
    for text in [
        "first.child.name",
        "child.name",
        "original.child.name",
        "view.child.name",
    ] {
        assert_eq!(expression(&module, text).ty, TypeHint::Int, "{text}");
    }
    assert_eq!(expression(&module, "number.value").ty, TypeHint::Float);
}

#[test]
fn mutations_through_receiver_captures_and_branches_widen_actual_aliases() {
    let module = module(
        r#"
fn main(flag:bool){
 let values=[1];values.append("new");let appended=values[1];
 let state={name:"old"};let change=||{state.name=12;};change();let captured=state.name;
 let child={name:"old"};let root={child:child};if flag{child.name=12;}child.name=true;let branched=root.child.name;
 (appended,captured,branched)
}
"#,
    );
    assert_eq!(expression(&module, "values[1]").ty, TypeHint::Any);
    assert_eq!(expression(&module, "state.name").ty, TypeHint::Any);
    assert_eq!(expression(&module, "root.child.name").ty, TypeHint::Bool);
}

#[test]
fn loops_keep_zero_iteration_returns_and_array_child_aliases() {
    let module = module(
        r#"
fn conditional(flag:bool){while flag{return 1;} "done"}
fn broken(){loop {break;} "after"}
fn returning(){loop {return 1;} "unreachable"}
fn main(flag:bool){
 let child={name:"old"};let rows=[child];for row in rows {row.name=12;}let iterated=child.name;
 conditional(flag);broken();returning();iterated
}
"#,
    );
    let result = &expression(&module, "conditional(flag)").ty;
    assert!(
        matches!(result,TypeHint::Union(types) if types.contains(&TypeHint::Int)&&types.contains(&TypeHint::String)),
        "{result:?}"
    );
    assert_eq!(expression(&module, "broken()").ty, TypeHint::String);
    assert_eq!(expression(&module, "returning()").ty, TypeHint::Int);
    assert_ne!(expression(&module, "child.name").ty, TypeHint::String);
}

#[test]
fn shared_closure_captures_and_tuple_types_stay_bounded() {
    let mut source =
        String::from("fn main(flag:bool){let state={value:1};let cb=||state.value;let t0=(1,1);");
    for i in 1..40 {
        source.push_str(&format!("let t{i}=(t{},t{});if flag{{}};", i - 1, i - 1));
    }
    source.push_str("cb();state.value}");
    let module = module(&source);
    assert_eq!(expression(&module, "state.value").ty, TypeHint::Any);
    assert!(
        module.stats.work_units < 20_000,
        "{}",
        module.stats.work_units
    );
}

#[test]
fn summary_budget_falls_back_without_changing_strict_compilation() {
    let mut source = String::from("fn parts(flag:bool){let value={");
    for i in 0..1600 {
        source.push_str(&format!("field{i}:{i},"));
    }
    source.push_str("};if flag{value.field0=1;}else{value.field0=2;}value}");
    let strict =
        aside_compiler::analyze_module(&source, &AnalysisOptions::default(), &mut NoModules)
            .unwrap();
    assert!(strict.is_ok(), "{:?}", strict.diagnostics);
    assert_eq!(strict.module().unwrap().source_types().work_units(), 0);
    let parsed = aside_compiler::parse(&source).unwrap();
    let analysis =
        aside_compiler::editor::analyze(&parsed, &AnalysisOptions::default(), &mut NoModules);
    assert!(
        !analysis.exhausted,
        "the optional summary cap preserves bound HIR"
    );
    assert_eq!(analysis.module().unwrap().source_types().work_units(), 0);
    assert!(analysis.stats.work_units < strict.stats.work_units + 1_002_000);
    let options = AnalysisOptions {
        limits: AnalysisLimits {
            max_work: strict.stats.work_units + 1000,
            ..Default::default()
        },
        ..Default::default()
    };
    let limited = aside_compiler::editor::analyze(&parsed, &options, &mut NoModules);
    assert!(
        limited.exhausted,
        "the caller's real shared analysis budget still applies"
    );
}

#[test]
fn function_and_global_initializer_chains_share_the_depth_bound() {
    let mut source = String::new();
    for i in 0..160 {
        source.push_str(&format!("fn f{i}(){{f{}()}}", i + 1));
    }
    source.push_str("fn f160(){7} fn main(){f0()}");
    let analysis = module(&source);
    assert_eq!(expression(&analysis, "f0()").ty, TypeHint::Any);
    assert!(analysis.module().unwrap().source_types().work_units() < 20_000);
    let mut source = String::new();
    for i in 0..80 {
        source.push_str(&format!("let g{i}=f{i}();fn f{i}(){{g{}}}", i + 1));
    }
    source.push_str("let g80=7;fn main(){g0}");
    let analysis = module(&source);
    assert_eq!(expression(&analysis, "g0").ty, TypeHint::Any);
}

#[test]
fn exported_global_records_keep_shared_aliases_through_module_interfaces() {
    let parsed =
        aside_compiler::parse(r#"pub let child={name:"old"};pub let alias=child;"#).unwrap();
    let ui = aside_compiler::editor::analyze(
        &parsed,
        &AnalysisOptions {
            module: "ui".into(),
            ..Default::default()
        },
        &mut NoModules,
    );
    assert!(ui.diagnostics.is_empty(), "{:?}", ui.diagnostics);
    let mut resolver = MemoryResolver::default();
    resolver.insert(
        ui.module()
            .unwrap()
            .source_types()
            .module_interface()
            .unwrap()
            .clone(),
    );
    let parsed =
        aside_compiler::parse(r#"use ui::{child,alias};fn main(){alias.name=12;child.name}"#)
            .unwrap();
    let analysis =
        aside_compiler::editor::analyze(&parsed, &AnalysisOptions::default(), &mut resolver);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(expression(&analysis, "child.name").ty, TypeHint::Int);
}

#[test]
fn escaped_function_results_remain_dynamic_after_known_writes_and_imports() {
    let source = r#"fn opaque(value:table){} pub fn make(){let r={name:"old"};opaque(r);r}
fn main(){let r=make();r.name="new";opaque({});r.name}"#;
    let analysis = module(source);
    assert_eq!(expression(&analysis, "r.name").ty, TypeHint::Any);
    let mut resolver = MemoryResolver::default();
    resolver.insert(
        analysis
            .module()
            .unwrap()
            .source_types()
            .module_interface()
            .unwrap()
            .clone(),
    );
    let parsed =
        aside_compiler::parse(r#"use main::make;fn consumer(){let r=make();r.name="new";r.name}"#)
            .unwrap();
    let analysis = aside_compiler::editor::analyze(
        &parsed,
        &AnalysisOptions {
            module: "other".into(),
            ..Default::default()
        },
        &mut resolver,
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(expression(&analysis, "r.name").ty, TypeHint::Any);
}

#[test]
fn unproven_handler_and_match_returns_remain_dynamic() {
    let analysis = module(
        r#"fn recover(){try{return 1;}catch error{return "caught";}}
fn choose(flag:bool){match flag{true=>{return 1;},false=>{return "other";}}}
fn main(){recover();choose(true)}"#,
    );
    assert_eq!(expression(&analysis, "recover()").ty, TypeHint::Any);
    assert_eq!(expression(&analysis, "choose(true)").ty, TypeHint::Any);
}
