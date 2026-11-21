use super::*;

fn panel(vm: &SessionVm) -> &reel_ui::stage::StageNode {
    vm.stage()
        .nodes()
        .find(|node| node.kind == NodeKind::Text("offset".into()))
        .unwrap()
}
fn advance(vm: &mut SessionVm, session: &mut Session) {
    let interaction = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(interaction))
            .unwrap();
    }
    drive(vm, session, false);
}

#[test]
fn scope_offset_sums_after_overrides_preserves_base_bindings_and_restores() {
    let source = r#"fn opening() {
        let data=model({x:4,y:8,shake:2});
        let panel=text("offset",x:derive(||data.x*2),y:bind(data.y));
        stage.replace(panel);
        scope(body: |s| {
            let lower=s.offset(panel,x:3,y:derive(||data.shake));
            data.x=5;data.y=10;data.shake=4;
            narrator("lower");
            if panel.x!=10 || panel.y!=10 || lower.x!=3 || lower.y!=4 {throw "base values";}
            let cover=s.override(panel,{x:100});
            scope(body: |inner| {
                let upper=inner.offset(panel,x:-2,y:1);
                lower.x=7;
                narrator("sum");
                let remove=lower.remove;remove();remove();
                narrator("lower removed");
                cover.remove();
                narrator("cover removed");
            });
            narrator("inner removed");
        });
        narrator("closed");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(source, encoded);
        drive(&mut vm, &mut session, false);
        for (x, y) in [
            (13., 14.),
            (105., 15.),
            (98., 11.),
            (8., 11.),
            (10., 10.),
            (10., 10.),
        ] {
            assert_eq!((panel(&vm).props.x, panel(&vm).props.y), (x, y));
            (session, vm) = restored(&mut vm, &session);
            assert_eq!((panel(&vm).props.x, panel(&vm).props.y), (x, y));
            advance(&mut vm, &mut session);
        }
        assert!(vm.scopes.live.is_empty());
    }
}

#[test]
fn scope_offset_motion_is_independent_and_exit_exposes_continuing_base_motion() {
    let source = r#"fn opening() {
        let panel=text("offset",x:0);stage.replace(panel);
        let moving=tween(panel,x:100,duration:10).run();
        scope(body: |s| {
            let shake=s.offset(panel,x:10,y:-3);
            let shaking=tween(shake,x:30,duration:10).run();
            narrator("both moving");
            if panel.x!=50 || shake.x!=20 {throw "independent motion";}
        });
        narrator("base moving");
        moving.finish();await moving;
        narrator("base finished");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(source, encoded);
        drive(&mut vm, &mut session, false);
        super::super::activities::advance_clock(&mut vm, &session, 2., 1);
        vm.sync_stage(&session).unwrap();
        assert_eq!((panel(&vm).props.x, panel(&vm).props.y), (34., -3.));
        (session, vm) = restored(&mut vm, &session);
        super::super::activities::advance_clock(&mut vm, &session, 5., 1);
        vm.sync_stage(&session).unwrap();
        assert_eq!((panel(&vm).props.x, panel(&vm).props.y), (70., -3.));
        advance(&mut vm, &mut session);
        assert_eq!((panel(&vm).props.x, panel(&vm).props.y), (50., 0.));
        super::super::activities::advance_clock(&mut vm, &session, 8., 1);
        vm.sync_stage(&session).unwrap();
        assert_eq!(panel(&vm).props.x, 80.);
        advance(&mut vm, &mut session);
        assert_eq!(panel(&vm).props.x, 100.);
    }
}

#[test]
fn scope_offset_rejects_invalid_fields_and_keeps_shared_source_conflicts() {
    let source = r#"fn opening() {
        let panel=text("offset");stage.replace(panel);
        let source=model({x:0});
        scope(body: |s| {
            let mut rejected=0;
            try{s.offset(panel,x:null);}catch error{rejected+=1;}
            try{s.offset(panel,y:"bad");}catch error{rejected+=1;}
            let first=s.offset(panel,x:bind(source.x));
            let second=s.offset(panel,x:bind(source.x));
            try{first.width=3;}catch error{rejected+=1;}
            try{tween(first,opacity:0,duration:1);}catch error{rejected+=1;}
            let moving=tween(first,x:40,duration:4).run();
            source.x=10;
            if await moving!="Stopped" {throw "source conflict";}
            let a=tween(first,x:20,duration:4).run();
            let b=tween(second,x:30,duration:4).run();
            if await a!="Stopped" {throw "shared source writer";}
            b.stop();await b;
            if rejected!=4 || first.x!=10 || second.x!=10 {throw "offset validation";}
            narrator("two bound layers");
        });
        narrator("removed");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(source, encoded);
        drive(&mut vm, &mut session, false);
        assert_eq!(panel(&vm).props.x, 20.);
        advance(&mut vm, &mut session);
        assert_eq!(panel(&vm).props.x, 0.);
    }
}

#[test]
fn scope_offset_fault_cancel_and_target_retirement_share_layer_cleanup() {
    let source = r#"fn opening() {
        let panel=text("offset",x:2);let root=stack(){panel};stage.replace(root);
        let state={layer:null,motion:null};let ready=completion();
        let task=spawn(||scope(body:|s|{
            state.layer=s.offset(panel,x:8);
            state.motion=tween(state.layer,x:20,duration:10).run(owner:root);
            ready.resolve();await completion();
        }));
        await ready;narrator("save");
        panel.remove();task.cancel();
        let mut cancelled=0;
        for work in [task,state.motion] {try{await work;}catch error{cancelled+=1;}}
        let mut retired=false;try{state.layer.x=1;}catch error{retired=true;}
        if cancelled!=2 || !retired {throw "retirement";}
        let other=root.add(text("offset",x:6));
        let original={reason:"failure"};let mut caught=false;
        try{scope(body:|s|{s.offset(other,x:30);throw original;});}
        catch error{caught=error==original;}
        if !caught {throw "fault identity";}
        narrator("cleaned");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(source, encoded);
        drive(&mut vm, &mut session, false);
        assert_eq!(panel(&vm).props.x, 10.);
        (session, vm) = restored(&mut vm, &session);
        advance(&mut vm, &mut session);
        assert_eq!(panel(&vm).props.x, 6.);
        assert!(vm.scopes.live.is_empty());
        assert!(!vm.activities.has_active());
    }
}
