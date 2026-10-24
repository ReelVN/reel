use super::models::{drive, prepared};
use super::*;
use crate::aside::{GeometryReply, NodeGeometry};
use reel_ui::stage::NodeKind;
fn done(vm: &SessionVm) -> bool {
    vm.stage()
        .nodes()
        .any(|node| node.kind == NodeKind::Text("done".into()))
}
fn reply(sequence: u64) -> GeometryReply {
    GeometryReply {
        sequence,
        geometry: Some(NodeGeometry {
            width: 20.,
            height: 30.,
            bounds: [10., 11., 20., 30.],
            transform: [1., 0., 0., 1., 10., 11.],
            text_end: Some([25., 41.]),
            text_caret: Some([25., 11., 0., 30.]),
        }),
    }
}
#[test]
fn geometry_results_wait_for_geometry_and_acceptance_and_are_readonly_and_reusable() {
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(
            r#"use reel::types::{GeometryRequest,Presentation,Geometry,Bounds,Transform,Point};
            fn change_x(value){value.x=0;}
            fn inspect(g:Geometry,b:Bounds,t:Transform,p:Point)->float{g.width+b.x+t.tx+p.y}
            fn opening(){
            let target=stack(width:20,height:30);stage.replace(target);
            let request:GeometryRequest=stage.geometry(target);
            let presentation:Presentation=request;
            let g:Geometry=await request;
            if inspect(g,g.bounds,g.transform,g.text_end)!=81.0 {throw "typed geometry";}
            if g.width!=20 || g.height!=30 || g.bounds.x!=10 || g.transform.tx!=10 {throw "wrong geometry";}
            if g.text_end.x!=25 || g.text_end.y!=41 {throw "wrong text endpoint";}
            if g.text_caret.x!=25 || g.text_caret.y+g.text_caret.height/2.0!=26.0 {throw "wrong caret center";}
            let mut readonly=false;try{change_x(g.bounds);}catch e{readonly=true;}
            if !readonly {throw "mutable geometry";}
            readonly=false;try{change_x(g.text_caret);}catch e{readonly=true;}
            if !readonly {throw "mutable caret";}
            readonly=false;try{change_x(g.text_end);}catch e{readonly=true;}
            if !readonly {throw "mutable text endpoint";}
            target.remove();
            if (await request).bounds.x!=10 || (await request).text_end.y!=41 || request.cancel(){throw "terminal result lost";}
            stage.replace(text("done"));
        }"#,
            encoded,
        );
        drive(&mut vm, &mut session, true);
        collect(&mut vm);
        let sequence = vm.geometry_queries().next().unwrap().unwrap().sequence;
        vm.accept_presentation(&session, sequence).unwrap();
        drive(&mut vm, &mut session, true);
        assert!(!done(&vm), "watermark alone cannot invent geometry");
        let identity = vm.stage().identity().clone();
        vm.accept_geometry(&identity, &[reply(sequence + 1)])
            .unwrap();
        drive(&mut vm, &mut session, true);
        assert!(!done(&vm));
        vm.accept_geometry(&identity, &[reply(sequence)]).unwrap();
        assert!(
            vm.geometry_queries().next().is_none(),
            "an accepted outcome is frozen while delivery is pending"
        );
        let mut later = reply(sequence);
        later.geometry.as_mut().unwrap().width = 99.;
        vm.accept_geometry(&identity, &[later]).unwrap();
        drive(&mut vm, &mut session, true);
        assert!(done(&vm));
        collect(&mut vm);
        assert!(vm.geometry_queries().next().is_none());
    }
}
#[test]
fn geometry_guards_cancel_without_a_frame_and_unavailable_rejects() {
    for encoded in [false, true] {
        for action in [
            "target.remove()",
            "relative.remove()",
            "owner.remove()",
            "request.cancel()",
        ] {
            let code = format!(
                r#"fn opening(){{
                let target=stack();let relative=stack();let owner=stack();
                stage.replace(stack{{target;relative;owner;}});
                let request=stage.geometry(target,relative_to:relative,owner:owner);
                {action};let mut cancelled=false;
                try{{await request;}}catch e{{cancelled=true;}}
                if !cancelled{{throw "guard did not cancel";}}stage.replace(text("done"));
            }}"#
            );
            let (mut session, mut vm) = prepared(&code, encoded);
            drive(&mut vm, &mut session, true);
            assert!(done(&vm), "{action}");
        }
        let (mut session, mut vm) = prepared(
            r#"fn opening(){let target=stack();stage.replace(target);
            let mut rejected=false;try{await stage.geometry(target);}catch e{rejected=e.code=="GeometryUnavailable";}
            if !rejected{throw "wrong unavailable error";}stage.replace(text("done"));}"#,
            encoded,
        );
        drive(&mut vm, &mut session, true);
        let sequence = vm.geometry_queries().next().unwrap().unwrap().sequence;
        let identity = vm.stage().identity().clone();
        vm.accept_geometry(
            &identity,
            &[GeometryReply {
                sequence,
                geometry: None,
            }],
        )
        .unwrap();
        vm.accept_presentation(&session, sequence).unwrap();
        drive(&mut vm, &mut session, true);
        assert!(done(&vm));
    }
}
#[test]
fn geometry_pending_snapshot_discards_old_identity_and_waits_for_new_geometry() {
    use aside_vm::snapshot::SnapshotBudget;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(
            r#"fn opening(){let target=stack();stage.replace(target);
            let g=await stage.geometry(target);if g.width!=20 || g.text_end.x!=25{throw "geometry changed";}stage.replace(text("done"));}"#,
            encoded,
        );
        drive(&mut vm, &mut session, true);
        let old = vm.stage().identity().clone();
        let sequence = vm.geometry_queries().next().unwrap().unwrap().sequence;
        vm.accept_geometry(&old, &[reply(sequence)]).unwrap();
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let bytes = capture.finish().unwrap().encode().unwrap();
        let mut saved = snapshot::SessionSnapshot::decode(&bytes).unwrap();
        let mut restore = SessionVm::restore_begin(
            &mut saved,
            vm.vm.program().clone(),
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(71),
                epoch: Epoch(19),
            },
            Default::default(),
            None,
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut session, mut restored) = restore.finish().unwrap();
        restored.accept_geometry(&old, &[reply(sequence)]).unwrap();
        restored.accept_presentation(&session, sequence).unwrap();
        drive(&mut restored, &mut session, true);
        assert!(!done(&restored));
        let identity = restored.stage().identity().clone();
        restored
            .accept_geometry(&identity, &[reply(sequence)])
            .unwrap();
        restored.accept_presentation(&session, sequence).unwrap();
        drive(&mut restored, &mut session, true);
        assert!(done(&restored));
    }
}

#[test]
fn geometry_frame_payload_is_charged_until_the_frame_is_released() {
    let (mut session, mut vm) = prepared(
        r#"fn opening(){let target=stack();stage.replace(target);
        let mut i=0;while i<20{stage.geometry(target);i+=1;}narrator("waiting");}"#,
        true,
    );
    drive(&mut vm, &mut session, true);
    let usage = vm.vm.heap().budget_usage();
    let before = usage.charged_bytes();
    let replies = vm
        .record_geometry(vm.presentation_request(), |query| {
            reply(query.sequence).geometry
        })
        .unwrap();
    assert_eq!(replies.len(), 20);
    assert!(usage.charged_bytes() >= before + 20 * size_of::<GeometryReply>() as u64);
    let second = vm
        .record_geometry(vm.presentation_request(), |_| None)
        .unwrap();
    assert!(usage.charged_bytes() >= before + 40 * size_of::<GeometryReply>() as u64);
    drop(replies);
    drop(second);
    assert_eq!(usage.charged_bytes(), before);
}

#[test]
fn geometry_terminal_snapshot_preserves_the_number_after_target_retirement() {
    use aside_vm::snapshot::SnapshotBudget;
    let (mut session, mut vm) = prepared(
        r#"use reel::types::{GeometryRequest,Geometry,Bounds,Transform,Point};
        fn opening(){let target=stack();stage.replace(target);
        let request=stage.geometry(target);let first=await request;target.remove();narrator("checkpoint");
        let restored=await request;if restored.bounds.x!=10 || restored.width!=20{throw "terminal geometry lost";}
        if !(request is GeometryRequest) || !(restored is Geometry) || !(restored.bounds is Bounds)
            || !(restored.transform is Transform) || !(restored.text_end is Point){throw "geometry type lost";}
        stage.replace(text("done"));}"#,
        true,
    );
    drive(&mut vm, &mut session, true);
    let sequence = vm.geometry_queries().next().unwrap().unwrap().sequence;
    let identity = vm.stage().identity().clone();
    vm.accept_geometry(&identity, &[reply(sequence)]).unwrap();
    vm.accept_presentation(&session, sequence).unwrap();
    drive(&mut vm, &mut session, true);
    collect(&mut vm);
    let mut capture = vm.snapshot_begin(&session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let bytes = capture.finish().unwrap().encode().unwrap();
    let mut saved = snapshot::SessionSnapshot::decode(&bytes).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut saved,
        vm.vm.program().clone(),
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(71),
            epoch: Epoch(19),
        },
        Default::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (mut session, mut restored) = restore.finish().unwrap();
    assert!(restored.geometry_queries().next().is_none());
    let interaction = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(interaction))
            .unwrap();
    }
    drive(&mut restored, &mut session, true);
    assert!(done(&restored));
}
