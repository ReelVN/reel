use super::*;
use aside_vm::snapshot::SnapshotBudget;

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

fn position(vm: &mut SessionVm, session: &Session) -> (f64, f64, Option<f64>) {
    vm.sync_stage(session).unwrap();
    let props = &vm.stage().node(vm.stage().root().unwrap()).unwrap().props;
    (props.x, props.y, props.width)
}

#[test]
fn tween_custom_curve_shared_sampling_restore_and_controls() {
    let source = r#"use std::math::curve as scalar_curve;
    fn opening() {
        let curve=scalar_curve([
            {time:0,value:0,mode:"cubic",out_tangent:0.5},
            {time:1,value:1,in_tangent:1.5}]);
        let node=stack(x:0,y:0);stage.replace(node);
        let action=tween(node,x:100,y:200,duration:2,easing:curve);
        let moving=run(action);
        narrator("moving");
        if node.x!=37.5 || node.y!=75 {throw "accelerated midpoint";}
        pause(moving);
        narrator("paused");
        if node.x!=37.5 {throw "paused moved";}
        resume(moving);
        narrator("resumed");
        finish(moving);
        if await moving!="Completed" || node.x!=100 || node.y!=200 {throw "exact endpoint";}
        let stopped=run(tween(node,x:300,duration:2,easing:curve));
        stop(stopped);
        if await stopped!="Stopped" || node.x!=100 {throw "cancel jump";}
        let zero=run(tween(node,x:400,duration:0,easing:curve));
        if await zero!="Completed" || node.x!=400 {throw "zero duration";}
        let mut rejected=0;
        try {tween(node,x:0,easing:scalar_curve([{time:0,value:1}]));}
        catch error {rejected+=1;}
        try {tween(node,x:0,easing:{});} catch error {rejected+=1;}
        if rejected!=2 {throw "curve contract";}
        narrator("done");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        models::drive(&mut vm, &mut session, false);
        let chunks = vm.vm.heap().stats().chunk_allocations;
        for step in 1..=60 {
            advance_clock(&mut vm, &session, step as f64 / 60., 64);
        }
        assert_eq!(vm.vm.heap().stats().chunk_allocations, chunks);
        assert_eq!(position(&mut vm, &session).0, 37.5);
        (session, vm) = restore(&session, &mut vm);
        advance_dialogue(&mut vm, &mut session);
        advance_clock(&mut vm, &session, 5., 64);
        assert_eq!(position(&mut vm, &session).0, 37.5);
        advance_dialogue(&mut vm, &mut session);
        advance_clock(&mut vm, &session, 5.5, 64);
        assert_eq!(position(&mut vm, &session).0, 65.625);
        advance_dialogue(&mut vm, &mut session);
        assert!(matches!(session.view().interaction,
            Some(crate::InteractionView::Dialogue(d)) if d.text()=="done"));
    }
}

#[test]
fn tween_from_dynamic_record_delayed_repeat_and_snapshot() {
    let source = r#"fn opening() {
        let node=stack(x:7,y:3);stage.replace(node);
        let initial={x:0,width:10};
        let action=tween(node,from:initial,x:20,y:23,width:30,duration:2);
        initial.x=99;initial.width=100;
        let animation=run(sequence(delay(1),repeat(action,times:2)),paused:true);
        if node.x!=7 || node.width!=null {throw "description wrote initial state";}
        narrator("prepared");
        if node.x!=7 || node.width!=null {throw "paused changed state";}
        resume(animation);
        narrator("delayed");
        if node.x!=7 || node.width!=null {throw "delay wrote initial state";}
        node.y=13;
        narrator("waiting");
        if await animation!="Completed" {throw "completion";}
        if node.x!=20 || node.y!=23 || node.width!=30 {throw "final values";}
        narrator("done");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        models::drive(&mut vm, &mut session, false);
        advance_clock(&mut vm, &session, 5., 1);
        assert_eq!(position(&mut vm, &session), (7., 3., None));
        (session, vm) = restore(&session, &mut vm);
        advance_dialogue(&mut vm, &mut session);
        advance_clock(&mut vm, &session, 5.5, 1);
        advance_dialogue(&mut vm, &mut session);
        advance_clock(&mut vm, &session, 6., 1);
        assert_eq!(position(&mut vm, &session), (0., 13., Some(10.)));
        advance_clock(&mut vm, &session, 7., 1);
        assert_eq!(position(&mut vm, &session), (10., 18., Some(20.)));
        (session, vm) = restore(&session, &mut vm);
        advance_clock(&mut vm, &session, 8., 1);
        assert_eq!(position(&mut vm, &session), (0., 23., Some(10.)));
        advance_clock(&mut vm, &session, 9., 1);
        assert_eq!(position(&mut vm, &session), (10., 23., Some(20.)));
        advance_clock(&mut vm, &session, 10., 1);
        advance_dialogue(&mut vm, &mut session);
        assert!(matches!(session.view().interaction,
            Some(crate::InteractionView::Dialogue(d)) if d.text()=="done"));
    }
}

#[test]
fn tween_from_controls_takeover_and_scope_properties() {
    let source = r#"fn opening() {
        let state=model({position:7});
        let node=stack(x:bind(state.position));stage.replace(node);
        let old=run(tween(node,x:90,duration:10));
        let prepared=run(tween(node,from:{x:20},x:40,duration:1),paused:true);
        if state.position!=7 {throw "paused takeover";}
        resume(prepared);
        if await old!="Stopped" || state.position!=20 {throw "explicit start takeover";}
        node.x=node.x;
        if await prepared!="Stopped" || state.position!=20 {throw "external write";}
        let stopped=run(tween(node,from:{x:50},x:60),paused:true);
        stop(stopped);
        if await stopped!="Stopped" || state.position!=20 {throw "stop before start";}
        let forced=run(tween(node,from:{x:50},x:60),paused:true);
        finish(forced);
        if await forced!="Completed" || state.position!=60 {throw "finish";}
        if await run(tween(node,from:{x:70},x:80,duration:0))!="Completed"
            || state.position!=80 {throw "zero";}
        scope(body:|s| {
            let offset=s.offset(node,x:5);
            let moving=s.run(tween(offset,from:{x:10},x:30));
            if offset.x!=10 || node.x!=80 {throw "offset initial";}
            finish(moving);
            if offset.x!=30 {throw "offset endpoint";}
            let layer=s.override(node,{opacity:0.4});
            let fade=s.run(tween(layer,from:{opacity:0.1},opacity:0.8));
            if layer.opacity!=0.1 || node.opacity!=1 {throw "override initial";}
            finish(fade);
            if layer.opacity!=0.8 {throw "override endpoint";}
        });
        let retired=run(tween(node,from:{x:90},x:100),owner:node);
        node.remove();
        let mut cancelled=false;
        try {await retired;} catch error {cancelled=true;}
        if !cancelled {throw "retirement";}
        narrator("done");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        models::drive(&mut vm, &mut session, false);
        assert!(matches!(session.view().interaction,
            Some(crate::InteractionView::Dialogue(d)) if d.text()=="done"));
        assert!(!vm.activities_need_time());
    }
}

#[test]
fn tween_from_invalid_records_and_linked_constraints_preserve_current_activity() {
    let source = r#"fn opening() {
        let state=model({value:0.3});
        let node=stack(x:bind(state.value),opacity:bind(state.value));
        stage.replace(node);
        let old=run(tween(node,x:0.8,duration:1));
        let mut rejected=0;
        try {tween(node,from:{unknown:1},x:1);} catch error {rejected+=1;}
        try {tween(node,from:{y:1},x:1);} catch error {rejected+=1;}
        try {tween(node,from:{x:1},x:null);} catch error {rejected+=1;}
        try {tween(node,from:{x:null},x:1);} catch error {rejected+=1;}
        try {tween(node,from:{x:"bad"},x:1);} catch error {rejected+=1;}
        try {tween(node,from:[],x:1);} catch error {rejected+=1;}
        try {tween(node,from:{opacity:2},opacity:1);} catch error {rejected+=1;}
        try {tween(node,from:{width:-1},width:1);} catch error {rejected+=1;}
        try {run(tween(node,from:{x:2},x:0.5));} catch error {rejected+=1;}
        try {run(parallel(tween(node,from:{x:0},x:1),
            tween(node,from:{opacity:0},opacity:1)));} catch error {rejected+=1;}
        if rejected!=10 || state.value!=0.3 {throw "invalid admission changed state";}
        finish(old);
        if await old!="Completed" || state.value!=0.8 {throw "invalid takeover";}
        let camera_node=camera();node.add(camera_node);
        try {tween(camera_node,from:{zoom:0},zoom:2);} catch error {rejected+=1;}
        if rejected!=11 {throw "camera start constraints";}
        let view=run(tween(camera_node,from:{camera_x:10,zoom:2},camera_x:30,zoom:3));
        if camera_node.camera_x!=10 || camera_node.zoom!=2 {throw "camera start";}
        finish(view);
        if camera_node.camera_x!=30 || camera_node.zoom!=3 {throw "camera end";}
        let viewport=scroll(width:100,height:100) {stack(height:500);};
        node.add(viewport);
        let scrolling=run(tween(viewport,from:{scroll_y:40,opacity:0.2},
            scroll_y:80,opacity:1));
        if viewport.scroll_y!=40 || viewport.opacity!=0.2 {throw "scroll start";}
        finish(scrolling);
        if viewport.scroll_y!=80 || viewport.opacity!=1 {throw "scroll end";}
        narrator("done");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        models::drive(&mut vm, &mut session, false);
        assert!(matches!(session.view().interaction,
            Some(crate::InteractionView::Dialogue(d)) if d.text()=="done"));
    }
}

#[test]
fn tween_from_repeated_activation_reuses_tracks_without_vm_allocation() {
    let source = r#"fn opening() {
        let node=stack(x:5,y:5);stage.replace(node);
        run(repeat(tween(node,from:{x:0,y:0},x:100,y:200,duration:1)));
        narrator("running");
    }"#;
    let (mut session, mut vm) = models::prepared(source, false);
    models::drive(&mut vm, &mut session, false);
    let chunks = vm.vm.heap().stats().chunk_allocations;
    for frame in 1..=120 {
        advance_clock(&mut vm, &session, frame as f64 / 60., 8);
    }
    assert_eq!(position(&mut vm, &session), (0., 0., None));
    assert_eq!(vm.vm.heap().stats().chunk_allocations, chunks);
}

#[test]
fn tween_from_repeat_publishes_complete_property_batch_to_derived_consumers() {
    let source = r#"fn opening() {
        let node=stack(x:5,y:10);
        let observer=text("observer",x:derive(||node.x*2-node.y));
        stage.replace(stack {node;observer;});
        run(repeat(tween(node,from:{x:0,y:0},x:100,y:200,duration:1)));
        narrator("running");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        models::drive(&mut vm, &mut session, false);
        for time in [0.5, 1., 1.5, 2., 2.5] {
            advance_clock(&mut vm, &session, time, 1);
            models::drive(&mut vm, &mut session, false);
            let observer = vm
                .stage()
                .nodes()
                .find(|node| node.kind == reel_ui::stage::NodeKind::Text("observer".into()))
                .unwrap();
            assert_eq!(observer.props.x, 0.);
        }
    }
}

#[test]
fn tween_from_unmatched_key_reports_the_authors_call() {
    let code = r#"fn opening() {
        let node=stack();stage.replace(node);
        tween(node,from:{y:1},x:2);
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        let id = loop {
            match next(&mut vm, &mut session) {
                RunOutcome::HostCall(ticket) => {
                    if let Err(error) =
                        vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(32))
                    {
                        vm.reject_native(&session, ticket, error).unwrap();
                    }
                }
                RunOutcome::Fault(id) => break id,
                outcome => panic!("expected invalid from fault, got {outcome:?}"),
            }
        };
        let fault = vm.vm.delivery_fault(id).unwrap().unwrap();
        let span = fault.location.source.unwrap();
        assert_eq!(
            &code[span.start as usize..span.end as usize],
            "tween(node,from:{y:1},x:2)"
        );
    }
}
