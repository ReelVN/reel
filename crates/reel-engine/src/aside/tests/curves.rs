use super::*;
use aside_vm::snapshot::SnapshotBudget;
use reel_ui::stage::NodeKind;

fn restore(session: &Session, vm: &mut SessionVm) -> (Session, SessionVm) {
    let program = vm.vm.program().clone();
    let mut capture = vm.snapshot_begin(session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut image =
        snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        program,
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(42),
            epoch: Epoch(session.identity().epoch.0 + 1),
        },
        crate::ReadProgress::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    restore.finish().unwrap()
}

fn click(vm: &mut SessionVm, session: &mut Session, label: &str) {
    let id = vm
        .stage()
        .nodes()
        .find(|node| matches!(&node.kind,NodeKind::Button{label:Some(value),..} if value==label))
        .unwrap()
        .id;
    assert!(vm.click(session, id).unwrap());
    models::drive(vm, session, false);
}

fn x(vm: &SessionVm) -> f64 {
    vm.stage()
        .nodes()
        .find(|node| node.kind == NodeKind::Text("sample".into()))
        .unwrap()
        .props
        .x
}

#[test]
fn scalar_curves_interpolation_discontinuities_copy_and_invalid_inputs_source_asb() {
    let source = r#"
    use std::math::{curve,pow};
    let initialized=curve([{time:-2,value:4},{time:2,value:8}]);
    fn near(a,b) { if a-b>0.000000001 || b-a>0.000000001 {throw "curve sample";} }
    fn opening() {
        near(initialized.sample(-10),4);near(initialized.sample(0),6);near(initialized.sample(10),8);
        let keys=[{time:0,value:0,mode:"cubic",out_tangent:2},{time:2,value:4,in_tangent:0}];
        let value=curve(keys);let sample=value.sample;
        keys[0].value=999;keys.append({time:9,value:42});
        near(sample(0),0);near(sample(1),2.5);near(sample(2),4);near(sample(0.5),1.1875);
        let jump=curve([
            {time:0,value:0},{time:1,value:1},{time:1,value:8,mode:"step"},
            {time:2,value:20},{time:2,value:30},{time:3,value:40}]);
        for time in [3,2.5,2,1.5,1,0.5,0,-1] {
            let expected=if time>=3 {40} else if time>=2 {30+(time-2)*10}
                else if time>=1 {8} else if time>=0 {time} else {0};
            near(jump.sample(time),expected);
        }
        let constant=curve(({time:-10,value:7},));near(constant.sample(999),7);
        let copied=model({curve:value});
        if copied.curve!=value {throw "immutable curve identity";}
        let mut rejected=0;
        for invalid in [[],{},[{}],[{time:0}],[{value:0}],
            [{time:1,value:0},{time:0,value:1}],[{time:0,value:0,mode:"unknown"}],
            [{time:0,value:0,typo:1}],[{time:pow(10.0,1000.0),value:0}],
            [{time:0,value:pow(-1.0,0.5)}],[{time:0,value:0,in_tangent:"bad"}]] {
            try {curve(invalid);} catch error {rejected+=1;}
        }
        try {sample(pow(10.0,1000.0));} catch error {rejected+=1;}
        try {value.sample=null;} catch error {rejected+=1;}
        if rejected!=13 {throw "curve argument boundary";}
        narrator("done");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        models::drive(&mut vm, &mut session, true);
        assert!(matches!(session.view().interaction,
            Some(crate::InteractionView::Dialogue(d)) if d.text()=="done"));
    }
}

#[test]
fn scalar_curves_conditional_sampling_and_dynamic_construction_survive_restore() {
    let source = r#"use std::math::curve;
    fn opening() {
        let state=model({visible:false,time:0,keys:[{time:0,value:0},{time:2,value:20}]});
        let motion=derive(||curve(state.keys));
        let hidden={offset:0};
        let node=text("sample",x:derive(||if state.visible {
            motion.value.sample(state.time)+hidden.offset
        } else {-1}));
        stage.replace(column(){node;
            button("show",on_click:|e|{state.visible=true;});
            button("seek",on_click:|e|{state.time=1;});
            button("hide",on_click:|e|{state.visible=false;});
            button("rewind",on_click:|e|{state.time=0.5;hidden.offset=100;});
            button("edit",on_click:|e|{state.keys[1].value=40;});
        });
        narrator("ready");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        models::drive(&mut vm, &mut session, true);
        assert_eq!(x(&vm), -1.);
        click(&mut vm, &mut session, "seek");
        assert_eq!(x(&vm), -1.);
        (session, vm) = restore(&session, &mut vm);
        click(&mut vm, &mut session, "show");
        assert_eq!(x(&vm), 10.);
        click(&mut vm, &mut session, "hide");
        click(&mut vm, &mut session, "rewind");
        assert_eq!(x(&vm), -1.);
        (session, vm) = restore(&session, &mut vm);
        click(&mut vm, &mut session, "show");
        assert_eq!(x(&vm), 105.);
        click(&mut vm, &mut session, "edit");
        assert_eq!(x(&vm), 110.);
    }
}

#[test]
fn scalar_curves_budgeted_construction_restores_and_detects_source_change() {
    for (encoded, change) in [(false, false), (true, false), (false, true), (true, true)] {
        let source = r#"use std::math::curve;
        fn opening() {
            let keys=[];
            for i in 0..129 {keys.append({time:i,value:i*2});}
            let mut value=null;
            try {value=curve(keys);} catch error {narrator("changed");return;}
            if value.sample(63.5)!=127 {throw "continued curve";}
            stage.replace(text("sample",x:value.sample(80.5)));
            narrator("done");
        }"#;
        let (mut session, mut vm) = models::prepared(source, encoded);
        let mut calls = 0;
        for _ in 0..100_000 {
            let outcome = vm
                .step(
                    &mut session,
                    SessionBudget::Exact(SliceBudget {
                        instructions: 3,
                        ..Default::default()
                    }),
                    || 0,
                )
                .unwrap();
            if let SessionStep::Vm(RunOutcome::HostCall(ticket)) = outcome {
                let package = vm.vm.program().package();
                let curve = package.symbols[package.natives[ticket.native.index()].symbol.index()]
                    == "std::math::curve";
                if curve {
                    calls += 1;
                }
                let source = if curve {
                    Some(vm.vm.host_arguments(ticket).unwrap()[0])
                } else {
                    None
                };
                vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(1))
                    .unwrap();
                if curve && (calls == 3 || !change && calls == 131) {
                    if change {
                        vm.vm.heap_mut().array_pop(source.unwrap()).unwrap();
                    } else {
                        (session, vm) = restore(&session, &mut vm);
                    }
                }
                if curve {
                    collect(&mut vm);
                }
            } else if matches!(outcome, SessionStep::Vm(RunOutcome::Waiting(_))) {
                break;
            } else if matches!(outcome, SessionStep::Vm(RunOutcome::Fault(_))) {
                panic!("curve failed: {outcome:?}");
            }
        }
        models::drive(&mut vm, &mut session, false);
        let expected = if change { "changed" } else { "done" };
        assert!(matches!(session.view().interaction,
            Some(crate::InteractionView::Dialogue(d)) if d.text()==expected));
        if !change {
            assert_eq!(x(&vm), 161.);
        }
    }
}

#[test]
fn scalar_curves_cancel_releases_capture_and_tree_workspaces() {
    let source = r#"use std::math::curve;
    fn opening() {
        let keys=[];
        for i in 0..129 {keys.append({time:i,value:i});}
        let value=curve(keys);
        narrator("unexpected completion");
    }"#;
    for cutoff in [3, 131] {
        let (mut session, mut vm) = models::prepared(source, true);
        collect(&mut vm);
        let before = vm.vm.heap().stats().live_objects;
        let mut calls = 0;
        while calls < cutoff {
            let outcome = vm
                .step(
                    &mut session,
                    SessionBudget::Exact(SliceBudget {
                        instructions: 3,
                        ..Default::default()
                    }),
                    || 0,
                )
                .unwrap();
            if let SessionStep::Vm(RunOutcome::HostCall(ticket)) = outcome {
                let package = vm.vm.program().package();
                let curve = package.symbols[package.natives[ticket.native.index()].symbol.index()]
                    == "std::math::curve";
                vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(1))
                    .unwrap();
                if curve {
                    calls += 1;
                }
            } else if matches!(outcome, SessionStep::Vm(RunOutcome::Fault(_))) {
                panic!("{outcome:?}");
            }
        }
        vm.revoke(&mut session).unwrap();
        while vm.cleanup_pending() {
            vm.cleanup_step(
                &mut session,
                32,
                SessionBudget::Exact(SliceBudget::default()),
                || 0,
            )
            .unwrap();
        }
        collect(&mut vm);
        assert_eq!(vm.vm.heap().stats().live_objects, before, "cutoff {cutoff}");
    }
}
