use super::*;
use aside_compiler::semantic::TypeHint;

#[test]
fn geometry_fields_are_readonly_in_source_and_have_no_constructor_defaults() {
    let api = source::api();
    for (record, fields) in [
        (
            "Geometry",
            &["width", "height", "bounds", "transform", "text_end"][..],
        ),
        ("Bounds", &["x", "y", "width", "height"]),
        ("Transform", &["a", "b", "c", "d", "tx", "ty"]),
        ("Point", &["x", "y"]),
    ] {
        for field in fields {
            let member = api
                .member(&format!("reel::types::{record}"), field)
                .unwrap();
            assert!(member.write_type.is_none());
            let descriptor = api
                .types
                .iter()
                .find(|ty| ty.name == format!("reel::types::{record}"))
                .unwrap()
                .members
                .iter()
                .find(|member| member.declaration.symbol.name == *field)
                .unwrap();
            assert!(descriptor.declaration.property_default.is_none());
            let code = format!(
                "use reel::types::{record}; fn change(value:{record}){{value.{field}=value.{field};}}"
            );
            assert!(source::compile_module(&code, "bad", "bad.aside", 1).is_err());
        }
    }
}

#[test]
fn callable_annotations_pass_functions_and_bound_methods_to_real_host_callbacks() {
    use crate::aside::transitions::TransitionPhase;
    for annotation in ["closure", "fn()->null"] {
        for callback in [
            "plain",
            "Scene().change",
            "||{game.record(\"closure\");null}",
        ] {
            let code = format!(
                r#"
            fn plain()->null {{game.record("plain");null}}
            class Scene {{fn change(self)->null {{game.record("bound");null}}}}
            fn run(change:{annotation}) {{stage.transition(change,duration:0);}}
            fn start(entry:{annotation}) {{game.start(entry);}}
            fn opening() {{
                let callback:{annotation}={callback};
                run(callback);
                if game.history().len()!=1 {{throw "callback was not invoked exactly once";}}
            }}
        "#
            );
            for encoded in [false, true] {
                let (mut session, mut vm) = models::prepared(&code, encoded);
                models::drive(&mut vm, &mut session, false);
                let view = vm
                    .transition_views()
                    .next()
                    .expect("transition awaits final presentation");
                assert_eq!(view.phase, TransitionPhase::FinalPending);
                assert!(
                    vm.accept_transition(view.id, view.phase, view.progress)
                        .unwrap()
                );
                models::drive(&mut vm, &mut session, false);
                assert!(vm.task().is_none());
                assert!(vm.transition_views().next().is_none());
            }
        }
    }
}

#[test]
fn registered_source_types_share_defaults_members_and_definition_locations() {
    let api = source::api();
    assert!(std::ptr::eq(api, source::api()));
    assert!(std::ptr::eq(source::contracts(), source::contracts()));
    let x = api.member("reel::types::TextNode", "x").unwrap();
    assert_eq!(x.symbol.ty, aside_host::source::number());
    let write = x.write_type.unwrap();
    assert!(
        matches!(write,TypeHint::Union(ref types) if types.contains(&TypeHint::Named("Binding".into())))
    );
    assert!(
        api.member("reel::types::Task", "pending")
            .unwrap()
            .write_type
            .is_none()
    );
    assert!(
        api.member("reel::types::VideoNode", "source")
            .unwrap()
            .write_type
            .is_none()
    );
    for field in ["source", "seed"] {
        assert!(
            api.member("reel::types::ParticlesNode", field)
                .unwrap()
                .write_type
                .is_none()
        );
    }
    assert_eq!(
        api.native("reel::video")
            .symbol
            .signature
            .as_ref()
            .unwrap()
            .parameters[0]
            .ty,
        TypeHint::String
    );
    assert_eq!(
        api.native("reel::layered_image")
            .symbol
            .signature
            .as_ref()
            .unwrap()
            .result,
        TypeHint::Named("reel::types::Node".into())
    );
    let append = api.member("reel::types::Speech", "append").unwrap().symbol;
    assert_eq!(
        append.signature.as_ref().unwrap().parameters[0].name,
        "text"
    );
    let documents = api.documents("reel");
    let append_location = documents
        .locations
        .iter()
        .find(|location| location.declaration.symbol.reference == append.reference)
        .unwrap();
    assert!(append_location.uri.ends_with("reel/types/Speech.aside"));
    let document = documents
        .documents
        .iter()
        .find(|document| document.uri == append_location.uri)
        .unwrap();
    assert!(document.text.contains("wait: bool = true"));
    assert!(!document.text.contains("speech:"));
    let x_location = documents
        .locations
        .iter()
        .find(|location| location.declaration.symbol.reference == x.symbol.reference)
        .unwrap();
    assert!(x_location.uri.ends_with("reel/types/Node.aside"));
    assert_eq!(
        api.is_subtype("reel::types::TextNode", "reel::types::Node"),
        Some(true)
    );
    assert_eq!(
        api.is_subtype("reel::types::TextNode", "reel::types::Speech"),
        Some(false)
    );
    let narrator = api
        .native("reel::narrator")
        .symbol
        .signature
        .as_ref()
        .unwrap();
    assert!(
        narrator
            .result_cases
            .iter()
            .any(|case| case.equals == bc::Const::Bool(true)
                && case.when_omitted
                && case.result == TypeHint::Null)
    );
    assert!(
        narrator
            .result_cases
            .iter()
            .any(|case| case.equals == bc::Const::Bool(false)
                && !case.when_omitted
                && case.result == TypeHint::Named("reel::types::Speech".into()))
    );
}

#[test]
fn host_node_binding_completion_task_and_scope_annotations_execute_from_source_and_asb() {
    let code = r#"use reel::types::{Node,TextNode,ButtonNode,Completion,Task,Scope,Action,Activity,Clock};
        fn identity(node:Node)->Node { node }
        fn scope_body(s:Scope) {
            let done:Completion=completion();
            if !done.pending || !done.resolve(7) || await done!=7 {throw "Completion";}
            let task:Task=s.spawn(|| 8);
            if await task!=8 {throw "Task";}
            let action:Action=delay(0);
            let running:Activity=s.run(action);
            finish(running);
            let pulse:Clock=clock(owner:s);
            if pulse.ticks!=0 || pulse.elapsed!=0.0 {throw "Clock";}
            stop(pulse);
            let state=model({x:3});
            let label:TextNode=text("typed",x:bind(state.x));
            let common:Node=identity(label);
            label.y=derive(|| state.x+1);
            let press:ButtonNode=button(on_click:|| {});
            let root:Node=column(){common;press;};
            let layers:Node=layered_image(|| []);
            root.add(layers);
            stage.replace(root);
            if label.x!=3 || label.y!=4 {throw "binding result";}
        }
        fn opening() {scope(scope_body);true}"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        assert!(vm.task().is_none());
    }
    for code in [
        "use reel::types::{Speech};fn opening(){let speech:Speech=text(\"wrong\");}",
        "fn opening(){let task=spawn(|| 1);task.pending=false;}",
    ] {
        assert!(
            source::compile_module(code, "bad", "bad.aside", 1).is_err(),
            "{code}"
        );
    }
}

#[test]
fn speech_literal_wait_results_and_bound_methods_execute_from_source_and_asb() {
    let code = r#"use reel::types::{Speech,Character,CharacterLine,RichText};
        fn opening() {
            let styled:RichText=rich("[b]hello[/b]");
            if styled.plain!="hello" {throw "RichText";}
            let who:Character=character("Lucy",id:"lucy");
            let line:CharacterLine=who.line("composed");
            let speech:Speech=narrator(styled,wait:false);
            speech.append(" more",wait:false);
            game.dialogue().clear();
            let from_character:Speech=who("spoken",wait:false);
            from_character.append(" tail",wait:false);
            game.dialogue().clear();
            let node:Node=text("default type name");
            let mut rejected=false;
            try {let wrong:Speech=dynamic(node);} catch error {rejected=true;}
            if !rejected {throw "wrong actual host type";}
            true
        }
        fn dynamic(value){value}"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        assert!(vm.task().is_none());
    }
    assert!(
        source::compile_module(
            "use reel::types::Speech;fn opening(){let speech:Speech=narrator(\"waits\");}",
            "bad",
            "bad.aside",
            1
        )
        .is_err()
    );
}
