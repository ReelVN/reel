use super::*;
use reel_ui::stage::NodeProps;

pub(super) fn drive(vm: &mut SessionVm, session: &mut Session) {
    for _ in 0..100_000 {
        match vm
            .step(
                session,
                SessionBudget::RetryMinimum(SliceBudget {
                    instructions: 7,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                if let Err(error) =
                    vm.dispatch(session, ticket, "en", NativeBudget::RetryMinimum(1))
                {
                    vm.reject_native(session, ticket, error).unwrap();
                }
                collect(vm);
            }
            SessionStep::Vm(RunOutcome::Fault(id)) => panic!(
                "box style fault: {:?}, value: {:?}",
                vm.vm.delivery_fault(id).unwrap(),
                text::read_string(vm.vm.heap(), vm.vm.delivery_value(id).unwrap(), 4096)
            ),
            SessionStep::Vm(RunOutcome::ResourceBlocked(error)) => {
                panic!("box style resource: {error:?}")
            }
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(session).unwrap(),
            SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_))
                if !vm.has_runnable_tasks() && !vm.activities_pending() =>
            {
                vm.sync_stage(session).unwrap();
                return;
            }
            _ => {}
        }
    }
    panic!("box style did not settle");
}

fn props(vm: &SessionVm) -> &NodeProps {
    &vm.stage().node(vm.stage().root().unwrap()).unwrap().props
}

fn advance(vm: &mut SessionVm, session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
    }
    drive(vm, session);
}

#[test]
fn box_style_source_asb_defaults_binding_writes_and_atomic_configuration_failure() {
    let code = r##"fn dynamic(value) {value} fn opening() {
        let defaults=stack();
        if defaults.border_width!=0 || defaults.corner_radius!=0 || defaults.border_color!="#ffffff" {
            throw "box defaults";
        }
        let state=model({width:2,radius:5,color:"#123456"});
        let other=model({width:99,radius:99,color:"#000000"});
        let node=stack(width:80,height:30,padding:3,border_width:bind(state.width),
            border_color:bind(state.color),corner_radius:bind(state.radius));
        stage.replace(node);
        if node.with(border_width:4,border_color:"#abcdef",corner_radius:7)!=node {
            throw "configuration changed identity";
        }
        node.border_color="#102030";
        if state.width!=4 || state.radius!=7 || state.color!="#102030" {throw "writeback";}
        let mut errors=0;
        try {node.with(border_width:bind(other.width),border_color:bind(other.color),corner_radius:-1);}
        catch error {errors+=1;}
        try {node.with(border_width:8,border_color:"invalid",corner_radius:20);}
        catch error {errors+=1;}
        if node.border_width!=4 || node.corner_radius!=7 || node.border_color!="#102030" {
            throw "failed configuration wrote values";
        }
        state.width=6;state.radius=9;state.color="#11223344";
        other.width=100;other.color="#ffffff";
        if node.border_width!=6 || node.corner_radius!=9 || node.border_color!="#11223344" {
            throw "failed configuration replaced bindings";
        }
        try {node.border_width=-1;} catch error {errors+=1;}
        try {node.corner_radius=dynamic(null);} catch error {errors+=1;}
        try {node.border_color=dynamic(null);} catch error {errors+=1;}
        try {let _ = stack(border_width:-1);} catch error {errors+=1;}
        try {let _ = stack(corner_radius:dynamic(null));} catch error {errors+=1;}
        try {let _ = stack(border_color:"#gg0000");} catch error {errors+=1;}
        let derived=stack(corner_radius:derive(|| state.radius*2));
        try {derived.with(corner_radius:1);} catch error {errors+=1;}
        if errors!=9 || state.width!=6 || state.radius!=9 || state.color!="#11223344" {
            throw "box validation changed previous values";
        }
        if derived.corner_radius!=18 {throw "derived box property";}
        node.with(border_width:1000,corner_radius:1000);
        if state.width!=1000 || state.radius!=1000 {throw "painting clamp changed authored values";}
        narrator("verified");
    }"##;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        drive(&mut vm, &mut session);
        let p = props(&vm);
        assert_eq!((p.border_width, p.corner_radius), (1000., 1000.));
        assert_eq!(p.border_color, [0x11, 0x22, 0x33, 0x44]);
        assert_eq!((p.width, p.height, p.padding), (Some(80.), Some(30.), 3.));
        (session, vm) = scopes::restored(&mut vm, &session);
        vm.sync_stage(&session).unwrap();
        assert_eq!(
            (props(&vm).border_width, props(&vm).corner_radius),
            (1000., 1000.)
        );
        assert_eq!(props(&vm).border_color, [0x11, 0x22, 0x33, 0x44]);
    }
}

#[test]
fn box_style_source_asb_tween_snapshot_completion_and_stop_preserve_bound_values() {
    let code = r##"fn opening() {
        let state=model({width:2,radius:4,color:"#8899aa"});
        let node=stack(width:80,height:40,padding:5,border_width:bind(state.width),
            corner_radius:bind(state.radius),border_color:bind(state.color));
        stage.replace(node);
        let moving=tween(node,border_width:10,corner_radius:20,duration:2).run();
        narrator("moving");
        if await moving!="Completed" || state.width!=10 || state.radius!=20 {throw "tween endpoint";}
        node.with(border_color:"#010203");
        if state.color!="#010203" {throw "restored color binding";}
        let stopped=tween(node,border_width:30,corner_radius:40,duration:2).run();
        narrator("stopping");
        stopped.stop();
        if await stopped!="Stopped" || state.width!=20 || state.radius!=30 {throw "stop lost progress";}
        state.color="#aabbcc";
        narrator("done");
    }"##;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        drive(&mut vm, &mut session);
        let id = vm.stage().root().unwrap();
        let chunks = vm.vm.heap().stats().chunk_allocations;
        for frame in 1..=60 {
            activities::advance_clock(&mut vm, &session, f64::from(frame) / 60., 64);
        }
        assert_eq!(vm.vm.heap().stats().chunk_allocations, chunks);
        vm.sync_stage(&session).unwrap();
        assert_eq!(
            (props(&vm).border_width, props(&vm).corner_radius),
            (6., 12.)
        );
        assert_eq!(props(&vm).border_color, [0x88, 0x99, 0xaa, 255]);
        (session, vm) = scopes::restored(&mut vm, &session);
        vm.sync_stage(&session).unwrap();
        assert_eq!(vm.stage().root(), Some(id));
        assert_eq!(
            (props(&vm).border_width, props(&vm).corner_radius),
            (6., 12.)
        );
        activities::advance_clock(&mut vm, &session, 2., 64);
        vm.sync_stage(&session).unwrap();
        assert_eq!(
            (props(&vm).border_width, props(&vm).corner_radius),
            (10., 20.)
        );
        advance(&mut vm, &mut session);
        activities::advance_clock(&mut vm, &session, 3., 64);
        vm.sync_stage(&session).unwrap();
        assert_eq!(
            (props(&vm).border_width, props(&vm).corner_radius),
            (20., 30.)
        );
        advance(&mut vm, &mut session);
        activities::advance_clock(&mut vm, &session, 10., 64);
        vm.sync_stage(&session).unwrap();
        assert_eq!(
            (props(&vm).border_width, props(&vm).corner_radius),
            (20., 30.)
        );
        assert_eq!(props(&vm).border_color, [0xaa, 0xbb, 0xcc, 255]);
        assert_eq!(
            (props(&vm).width, props(&vm).height, props(&vm).padding),
            (Some(80.), Some(40.), 5.)
        );
        assert!(!vm.activities_need_time());
    }
}

#[test]
fn box_style_schema_exposes_typed_writes_and_numeric_animation_targets() {
    use aside_compiler::semantic::TypeHint;
    let api = source::api();
    for owner in ["Node", "TextNode", "TextInputNode", "SliderNode"] {
        for (name, ty) in [
            ("border_width", aside_host::source::number()),
            ("border_color", TypeHint::String),
            ("corner_radius", aside_host::source::number()),
        ] {
            let member = api.member(&format!("reel::types::{owner}"), name).unwrap();
            assert_eq!(member.symbol.ty, ty);
            assert_eq!(
                member.write_type,
                Some(TypeHint::Union(vec![ty, TypeHint::Named("Binding".into())]))
            );
        }
    }
    for invalid in [
        "stack(border_width:null)",
        "stack(corner_radius:false)",
        "stack().with(border_color:null)",
        "tween(stack(),border_color:\"#ffffff\",duration:1)",
    ] {
        let code = format!("fn opening(){{let _ = {invalid};}}");
        assert!(
            source::compile_story(&code, "invalid-box-style", 1, "opening").is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn font_projection_reuses_text_and_tracks_binding_overrides_after_restore() {
    use std::sync::Arc;
    let code = r#"fn opening() {
        let state=model({font:"明朝体, system-ui"});
        let node=text("Caption",font:bind(state.font));
        stage.replace(node);
        narrator("initial");
        node.with(x:10,opacity:0.5);
        narrator("moved");
        state.font="黑体, system-ui";
        narrator("changed");
        scope(body:|page| {
            page.override(node,{font:"temporary"});
            narrator("covered");
        });
        narrator("restored");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        drive(&mut vm, &mut session);
        let font = props(&vm).font.clone();
        let copy = props(&vm).clone();
        assert!(Arc::ptr_eq(&font, &copy.font));
        advance(&mut vm, &mut session);
        assert!(Arc::ptr_eq(&font, &props(&vm).font));
        assert_eq!((props(&vm).x, props(&vm).opacity), (10., 0.5));
        advance(&mut vm, &mut session);
        assert_eq!(&*props(&vm).font, "黑体, system-ui");
        advance(&mut vm, &mut session);
        assert_eq!(&*props(&vm).font, "temporary");
        (session, vm) = scopes::restored(&mut vm, &session);
        vm.sync_stage(&session).unwrap();
        assert_eq!(&*props(&vm).font, "temporary");
        advance(&mut vm, &mut session);
        assert_eq!(&*props(&vm).font, "黑体, system-ui");
    }
}
