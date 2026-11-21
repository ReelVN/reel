use super::*;

fn advance(vm: &mut SessionVm, session: &mut Session) {
    let interaction = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(interaction))
            .unwrap();
    }
    drive(vm, session, false);
}
fn panel(vm: &SessionVm) -> &reel_ui::stage::StageNode {
    vm.stage()
        .nodes()
        .find(|node| node.kind == NodeKind::Text("covered".into()))
        .unwrap()
}

#[test]
fn scope_override_layers_preserve_latest_base_derive_null_and_snapshot() {
    let code = r#"fn opening() {
        let data=model({x:3,width:80,alpha:0.8});
        let panel=text("covered",x:derive(|| data.x*2),width:bind(data.width),opacity:0.9);
        stage.replace(stack() { panel; });
        scope(body: |s| {
            let lower=s.override(panel,{x:10,width:null,opacity:0.4});
            data.x=7; data.width=90;
            narrator("outer");
            if panel.x!=14 || panel.width!=90 || lower.x!=10 { throw "base access"; }
            scope(body: |inner| {
                let upper=inner.override(panel,{x:20,opacity:0.7});
                lower.x=15;
                narrator("inner");
                lower.remove();
                upper.opacity=derive(|| data.alpha);
                data.alpha=0.6;
                narrator("lower removed");
            });
            narrator("latest base");
        });
        narrator("closed");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        for (x, width, alpha) in [
            (10., None, 0.4),
            (20., None, 0.7),
            (20., Some(90.), 0.6),
            (14., Some(90.), 0.9),
            (14., Some(90.), 0.9),
        ] {
            let props = &panel(&vm).props;
            assert_eq!((props.x, props.width, props.opacity), (x, width, alpha));
            (session, vm) = restored(&mut vm, &session);
            let props = &panel(&vm).props;
            assert_eq!((props.x, props.width, props.opacity), (x, width, alpha));
            advance(&mut vm, &mut session);
        }
        assert!(vm.scopes.live.is_empty());
    }
}

#[test]
fn scope_override_rejects_nonappearance_fields_and_unprovided_animation_fields() {
    let code = r#"fn opening() {
        let panel=text("covered",x:2); stage.replace(panel);
        scope(body: |s| {
            let mut rejected=0;
            for props in [{hovered:true},{on_interaction:|| {}},{value:4},{scroll_y:3},{opacity:2},{unknown:1}] {
                try { s.override(panel,props); } catch error { rejected+=1; }
            }
            let effect=s.override(panel,{x:10});
            try { effect.y=2; } catch error { rejected+=1; }
            try { tween(effect,y:5,duration:1); } catch error { rejected+=1; }
            try { effect.remove=0; } catch error { rejected+=1; }
            if rejected!=9 { throw "override boundary"; }
            narrator("admitted");
        });
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        assert_eq!(panel(&vm).props.x, 10.);
        advance(&mut vm, &mut session);
        assert_eq!(panel(&vm).props.x, 2.);
    }
}

#[test]
fn scope_override_target_retirement_and_task_cancellation_release_layers() {
    let code = r#"fn opening() {
        let panel=text("covered",x:2); let root=stack() {panel;}; stage.replace(root);
        let entered=completion(); let state={effect:null,motion:null};
        let job=spawn(|| scope(body: |s| {
            state.effect=s.override(panel,{x:10});
            state.motion=tween(state.effect,x:20,duration:20).run(owner:root);
            entered.resolve(); await completion();
        }));
        await entered; narrator("save layer");
        panel.remove(); job.cancel();
        let mut cancelled=0;
        for work in [job,state.motion] { try { await work; } catch error { cancelled+=1; } }
        if cancelled!=2 { throw "layer work survived retirement"; }
        let mut retired=false; try { state.effect.x=1; } catch error { retired=true; }
        if !retired { throw "layer survived target"; }
        root.add(text("finished"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        (session, vm) = restored(&mut vm, &session);
        advance(&mut vm, &mut session);
        assert!(vm.scopes.live.is_empty());
        assert!(!vm.activities.has_active());
        assert_eq!(vm.stage().nodes().count(), 2);
    }
}

#[test]
fn scope_override_independent_and_hidden_animations_continue_after_restore() {
    let code = r#"fn opening() {
        let panel=text("covered",x:0); stage.replace(panel);
        let base=tween(panel,x:100,duration:10).run();
        scope(body: |s| {
            let lower=s.override(panel,{x:200});
            let hidden=tween(lower,x:300,duration:10).run(owner: s);
            let upper=s.override(panel,{x:400});
            let top=tween(upper,x:500,duration:10).run(owner: s);
            narrator("moving");
            if panel.x!=50 || lower.x!=250 || upper.x!=450 { throw "independent writers"; }
            upper.remove();
            try { await top; } catch error {}
            narrator("hidden continued");
            lower.remove();
            try { await hidden; } catch error {}
        });
        narrator("base continued");
        base.finish(); if await base!="Completed" || panel.x!=100 { throw "base animation"; }
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        super::super::activities::advance_clock(&mut vm, &session, 2., 1);
        vm.sync_stage(&session).unwrap();
        assert_eq!(panel(&vm).props.x, 420.);
        (session, vm) = restored(&mut vm, &session);
        // The captured activity clock remains at two seconds.
        super::super::activities::advance_clock(&mut vm, &session, 5., 1);
        vm.sync_stage(&session).unwrap();
        assert_eq!(panel(&vm).props.x, 450.);
        advance(&mut vm, &mut session);
        assert_eq!(panel(&vm).props.x, 250.);
        advance(&mut vm, &mut session);
        assert_eq!(panel(&vm).props.x, 50.);
        advance(&mut vm, &mut session);
        assert_eq!(panel(&vm).props.x, 100.);
    }
}

#[test]
fn scope_override_real_binding_sources_conflict_and_reject_derived_writes() {
    let code = r#"fn opening() {
        let data=model({x:0});
        let panel=text("covered",x:bind(data.x)); stage.replace(panel);
        scope(body: |s| {
            let effect=s.override(panel,{x:bind(data.x)});
            let first=tween(panel,x:100,duration:10).run();
            let second=tween(effect,x:200,duration:10).run();
            if await first!="Stopped" { throw "same source did not conflict"; }
            effect.x=5;
            if await second!="Stopped" || data.x!=5 || panel.x!=5 { throw "layer write through"; }
            effect.x=derive(|| data.x*2);
            let mut rejected=0;
            try { effect.x=8; } catch error { rejected+=1; }
            try { tween(effect,x:8).run(); } catch error { rejected+=1; }
            if rejected!=2 { throw "derived layer became writable"; }
            narrator("derived");
        });
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        assert_eq!(panel(&vm).props.x, 10.);
        advance(&mut vm, &mut session);
        assert_eq!(panel(&vm).props.x, 5.);
    }
}

#[test]
fn scope_override_hidden_choice_tracks_base_and_cancel_exposes_current_state() {
    let code = r#"fn opening() {
        let data=model({active:true});
        let panel=text("covered",visible:derive(|| data.active)); stage.replace(stack() {panel;});
        let entered=completion();
        let job=spawn(|| scope(body: |s| {
            s.override(panel,{visible:false}); entered.resolve(); await completion();
        }));
        await entered; narrator("covered active");
        data.active=false; narrator("covered inactive");
        data.active=true; narrator("covered active again");
        job.cancel(); try { await job; } catch error {}
        narrator("uncovered");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        for _ in 0..3 {
            assert!(!panel(&vm).props.visible);
            (session, vm) = restored(&mut vm, &session);
            assert!(!panel(&vm).props.visible);
            advance(&mut vm, &mut session);
        }
        assert!(panel(&vm).props.visible);
        assert!(vm.scopes.live.is_empty());
    }
}

#[test]
fn scope_override_cancellation_uses_existing_storage_and_idle_cost_ignores_layer_count() {
    for layers in [0, 1, 256] {
        let code = format!(
            r#"fn opening() {{
            let data=model({{x:2}}); let panel=text("covered",x:derive(|| data.x*2)); stage.replace(panel);
            scope(body: |s| {{
                let mut i=0;
                while i<{layers} {{ s.override(panel,{{x:bind(data.x),width:null}}); i+=1; }}
                narrator("retire layers");
            }});
        }}"#
        );
        let (mut session, mut vm) = prepared(&code, true);
        drive(&mut vm, &mut session, false);
        let before = vm.vm.heap().stats();
        let revision = vm.stage().revision();
        let started = std::time::Instant::now();
        for _ in 0..1000 {
            vm.sync_stage(&session).unwrap();
        }
        let elapsed = started.elapsed();
        assert_eq!(
            vm.vm.heap().stats().chunk_allocations,
            before.chunk_allocations
        );
        assert_eq!(vm.stage().revision(), revision);
        eprintln!(
            "override layers={layers}; 1000 idle projections={elapsed:?}; VM charged bytes={}; new VM chunks=0",
            before.charged_bytes
        );
        let interaction = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(interaction))
                .unwrap();
        }
        let mut measured = false;
        for _ in 0..1000 {
            match vm
                .step(
                    &mut session,
                    SessionBudget::Exact(SliceBudget::default()),
                    || 0,
                )
                .unwrap()
            {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    let package = vm.vm.program().package();
                    let closing = package.symbols
                        [package.natives[ticket.native.index()].symbol.index()]
                        == "reel::scope"
                        && ticket.input == NativeInput::CallbackReturned;
                    let before = vm.vm.heap().stats();
                    vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(32))
                        .unwrap();
                    if closing {
                        assert_eq!(
                            vm.vm.heap().stats().chunk_allocations,
                            before.chunk_allocations,
                            "override cleanup allocated"
                        );
                        measured = true;
                        break;
                    }
                }
                SessionStep::Vm(RunOutcome::BudgetYield(_)) | SessionStep::Progress => {}
                other => panic!("scope did not return: {other:?}"),
            }
        }
        assert!(measured);
        drive(&mut vm, &mut session, false);
        assert_eq!(panel(&vm).props.x, 4.);
    }
}

#[test]
fn scope_override_fault_and_owner_cancellation_unwind_to_latest_layer() {
    let code = r#"fn opening() {
        let panel=text("covered",x:1); let owner=stack(); stage.replace(stack() {panel;owner;});
        scope(body: |outer| {
            outer.override(panel,{x:10});
            let original={reason:"fault"}; let mut caught=false;
            try { scope(body: |s| { s.override(panel,{x:20}); throw original; }); }
            catch error { caught=error==original; }
            if !caught { throw "changed fault"; }
            narrator("after fault");
            try { scope(owner:owner,body: |s| {
                s.override(panel,{x:30}); owner.remove(); await completion();
            }); } catch error { if error!="ScopeCancelled" { throw error; } }
            narrator("after owner");
        });
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        assert_eq!(panel(&vm).props.x, 10.);
        (session, vm) = restored(&mut vm, &session);
        advance(&mut vm, &mut session);
        assert_eq!(panel(&vm).props.x, 10.);
        advance(&mut vm, &mut session);
        assert_eq!(panel(&vm).props.x, 1.);
    }
}

#[test]
fn scope_override_current_visibility_enabled_and_modal_guard_old_input() {
    let code = r#"fn opening() {
        let control=button("covered",on_click:|| {}); let other=button("other",on_click:|| {});
        stage.replace(stack() {control;other;});
        scope(body: |s| {
            let effect=s.override(control,{visible:false,enabled:true,modal:false});
            narrator("hidden");
            effect.visible=true; effect.enabled=false; narrator("disabled");
            effect.enabled=true; effect.modal=true; narrator("modal");
        });
        narrator("normal");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        let ids: Vec<_> = vm
            .stage()
            .nodes()
            .filter(|node| matches!(node.kind, NodeKind::Button { .. }))
            .map(|node| node.id)
            .collect();
        for expected in [(false, true), (false, true), (true, false), (true, true)] {
            assert_eq!(
                (
                    vm.stage.click(vm.vm.heap(), ids[0]).unwrap().is_some(),
                    vm.stage.click(vm.vm.heap(), ids[1]).unwrap().is_some()
                ),
                expected
            );
            advance(&mut vm, &mut session);
        }
    }
}

#[test]
fn scope_override_particle_capture_uses_effective_extent_before_projection() {
    let code = r#"fn opening() {
        let node=particles("snow.png",width:10,height:20); stage.replace(node);
        scope(body: |s| {
            s.override(node,{width:100,height:200});
            emit(node,burst:1,duration:1,lifetime:10,owner:node);
        });
        narrator("captured");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        let node = vm.stage().node(vm.stage().root().unwrap()).unwrap();
        assert_eq!(
            (node.props.width, node.props.height),
            (Some(10.), Some(20.))
        );
        let NodeKind::Particles(particles) = &node.kind else {
            panic!("particle node");
        };
        assert_eq!(particles.batches[0].frame.extent, [100., 200.]);
    }
}
