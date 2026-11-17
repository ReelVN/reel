use super::*;

#[test]
fn scope_finish_returns_original_value_and_retires_nested_work_before_continuing() {
    let code = r#"fn opening() {
        let root=stack();stage.replace(root);
        let expected={page:"title",visits:1};
        let state={finish:null,cancel:null,child:null,motion:null,after:false,cleanup:false};
        let returned=scope(body:|page| {
            state.finish=page.finish;state.cancel=page.cancel;
            let node=page.add(root,stack(){text("temporary");});
            state.child=page.spawn(||await completion());
            state.motion=page.run(tween(node,x:100,duration:10));
            scope(body:|inner| {
                inner.add(node,text("nested"));
                try {page.finish(expected);state.after=true;await completion();}
                finally {state.cleanup=true;}
            });
            throw "finished body continued";
        });
        if returned!=expected || state.after || state.cleanup {throw "finish boundary";}
        returned.visits+=1;
        if expected.visits!=2 || state.finish("late") || state.cancel() {throw "result identity";}
        let mut cancelled=0;
        for task in [state.child,state.motion] {try {await task;}catch error{cancelled+=1;}}
        if cancelled!=2 {throw "owned work survived";}
        root.add(text("returned to title"));
    }"#;
    for encoded in [false, true] {
        for restore_request in [false, true] {
            let (mut session, mut vm) = prepared(code, encoded);
            if restore_request {
                for _ in 0..10_000 {
                    if vm.scopes.live.iter().any(|scope| {
                        matches!(scope.exit, Some(crate::aside::scopes::ScopeExit::Finish))
                    }) {
                        break;
                    }
                    match vm
                        .step(
                            &mut session,
                            SessionBudget::Exact(SliceBudget {
                                instructions: 1,
                                ..Default::default()
                            }),
                            || 0,
                        )
                        .unwrap()
                    {
                        SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                            vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(32))
                                .unwrap();
                        }
                        SessionStep::Vm(RunOutcome::Returned(_)) => {
                            vm.discard_delivery(&mut session).unwrap()
                        }
                        SessionStep::Vm(RunOutcome::Fault(id)) => {
                            panic!("before finish: {:?}", vm.vm.delivery_fault(id))
                        }
                        _ => {}
                    }
                }
                assert!(vm.scopes.live.iter().any(|scope| {
                    matches!(scope.exit, Some(crate::aside::scopes::ScopeExit::Finish))
                }));
                collect(&mut vm);
                (session, vm) = restored(&mut vm, &session);
            }
            drive(&mut vm, &mut session, true);
            collect(&mut vm);
            assert!(vm.scopes.live.is_empty());
            assert!(vm.tasks.is_empty());
            assert!(!vm.activities.has_active());
            assert_eq!(vm.stage().nodes().count(), 2);
            assert!(
                vm.stage()
                    .nodes()
                    .any(|node| node.kind == NodeKind::Text("returned to title".into()))
            );
        }
    }
}

#[test]
fn waiting_scope_parks_until_input_finishes_it_after_restore() {
    let code = r#"fn opening() {
        let root = stack(); stage.replace(root);
        let result = scope(body: |page| {
            page.add(root, button("Close", on_click: || page.finish("closed")));
            page.wait();
            throw "parked body resumed";
        });
        if result != "closed" { throw "lost scope result"; }
        root.add(text(result));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        (session, vm) = restored(&mut vm, &session);
        let button = vm
            .stage()
            .nodes()
            .find(|node| {
                matches!(&node.kind,
            NodeKind::Button { label: Some(label), .. } if label == "Close")
            })
            .unwrap()
            .id;
        assert!(vm.click(&mut session, button).unwrap());
        drive(&mut vm, &mut session, true);
        assert!(vm.scopes.live.is_empty());
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("closed".into()))
        );
    }
}

#[test]
fn scope_finish_from_ui_replaces_a_waiting_dialogue_and_preserves_the_outer_flow() {
    let code = r#"fn opening() {
        let root=stack();stage.replace(root);
        let state={visits:0};
        for visit in 0..3 {
            let destination=scope(body:|page| {
                state.visits+=1;
                page.add(root,button("Leave",on_click:||page.finish("title")));
                narrator("A story line is waiting.");
                throw "retired dialogue resumed";
            });
            if destination!="title" || state.visits!=visit+1 {throw "page return";}
        }
        root.add(text("three returns"));
        narrator("The outer flow can speak again.");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        for visit in 0..3 {
            drive(&mut vm, &mut session, true);
            if visit == 1 {
                (session, vm) = restored(&mut vm, &session);
            }
            assert!(session.view().interaction.is_some());
            let button=vm.stage().nodes().find(|node|matches!(&node.kind,NodeKind::Button {label:Some(label),..} if label=="Leave")).unwrap().id;
            assert!(vm.click(&mut session, button).unwrap());
        }
        drive(&mut vm, &mut session, true);
        collect(&mut vm);
        assert!(vm.scopes.live.is_empty());
        assert!(vm.tasks.is_empty());
        assert_eq!(vm.stage().nodes().count(), 2);
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("three returns".into()))
        );
        assert!(session.view().interaction.is_some());
    }
}
