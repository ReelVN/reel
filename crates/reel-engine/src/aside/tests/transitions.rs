use super::{
    models::{drive, prepared},
    *,
};
use crate::aside::transitions::TransitionPhase;
use aside_vm::snapshot::SnapshotBudget;
use reel_ui::stage::NodeKind;
use std::sync::Arc;

fn retained_picture() -> (
    Arc<reel_ui::frame::RetainedScene>,
    Arc<reel_ui::frame::SavedRetainedScene>,
) {
    use reel_core::{
        budget::{BudgetCategory, BudgetPool, QueueLimits},
        identity::{FrameId, SurfaceEpoch},
    };
    use reel_ui::{
        app::{GeometryLimits, RenderRoot, RenderRootOptions, WindowSizePolicy},
        bridge::{BridgeLimits, PresentationBridge},
        core::{DefaultProperties, Widget},
        dpi::PhysicalSize,
        frame::{BuildLimits, FrameBuild, FrameInfo, RetainBuild, StepBudget},
        resources::ResourceRegistry,
        widgets::Slider,
    };
    let root = RenderRoot::new(
        Slider::new(0., 1., 0.5).prepare(),
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(400, 300),
            scale_factor: 1.,
            default_font: None,
        },
    );
    let mut bridge = PresentationBridge::new(
        root,
        SurfaceEpoch(1),
        BridgeLimits {
            geometry: GeometryLimits { max_bytes: 4 << 20 },
            max_candidates: 4,
            max_bridge_bytes: 64 << 10,
            max_snapshot_bytes: 24 << 20,
        },
    )
    .unwrap();
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 64,
            max_bytes: 64 << 20,
        },
    );
    let limits = BuildLimits {
        max_bytes: 16 << 20,
    };
    let budget = StepBudget {
        max_commands: 64,
        max_units: 8192,
        max_bytes: 64 << 10,
    };
    let source = bridge.record(FrameId(1)).unwrap();
    let resources = ResourceRegistry::new(0, 0)
        .unwrap()
        .freeze(&[], &pool)
        .unwrap();
    let mut build = FrameBuild::new(
        source,
        resources,
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        limits,
        &pool,
    )
    .unwrap();
    while !build.step(budget).unwrap().complete {}
    let frame = build.finish().unwrap();
    let mut retain = RetainBuild::new(&frame, None, limits, &pool).unwrap();
    while !retain.step(budget).unwrap().complete {}
    let picture = retain.finish().unwrap();
    let saved = picture.snapshot(|_| None).unwrap();
    (Arc::new(picture), Arc::new(saved))
}

fn accept_final(vm: &mut SessionVm) {
    let views: Vec<_> = vm.transition_views().collect();
    assert!(!views.is_empty());
    for view in views {
        assert_eq!(view.phase, TransitionPhase::FinalPending);
        assert_eq!(view.progress, 1.);
        assert!(
            vm.accept_transition(view.id, view.phase, view.progress, vm.logical_time())
                .unwrap()
        );
    }
}

#[test]
fn skipping_a_transition_finishes_once_after_the_final_frame() {
    let code = r#"fn opening() {
        let state = {calls: 0};
        stage.replace(stack());
        let result = stage.transition(duration: 10, skip: "finish", mask: "mask.png", change: || {
            state.calls += 1;
            stage.replace(text("new drawing"));
            await delay(1).run();
            41
        });
        if result != 41 || state.calls != 1 { throw "transition result"; }
        stage.replace(text("done"));
    }"#;
    let (picture, saved) = retained_picture();
    for encoded in [false, true] {
        for skip_at in [
            None,
            Some(TransitionPhase::OldPending),
            Some(TransitionPhase::Changing),
            Some(TransitionPhase::NewPending),
            Some(TransitionPhase::Playing),
        ] {
            let (mut session, mut vm) = prepared(code, encoded);
            if skip_at.is_none() {
                session.set_skip_reading(true, true, false);
            }
            drive(&mut vm, &mut session, false);
            let mut logical_time = 0.;
            let request = vm.take_transition_request().unwrap();
            if let Some(phase) = skip_at {
                if phase != TransitionPhase::OldPending {
                    let request = request.unwrap();
                    assert!(
                        vm.complete_transition_request(
                            request.id,
                            Ok((picture.clone(), saved.clone()))
                        )
                        .unwrap()
                    );
                    drive(&mut vm, &mut session, false);
                    if phase != TransitionPhase::Changing {
                        logical_time = 1.;
                        while vm.advance_time(&session, logical_time, 4).unwrap().pending {}
                        drive(&mut vm, &mut session, false);
                    }
                }
                if phase == TransitionPhase::Playing {
                    let view = vm.transition_views().next().unwrap();
                    assert!(
                        vm.accept_transition(
                            view.id,
                            TransitionPhase::NewPending,
                            0.,
                            logical_time
                        )
                        .unwrap()
                    );
                    logical_time += 0.25;
                    while vm.advance_time(&session, logical_time, 1).unwrap().pending {}
                }
                assert_eq!(vm.transition_views().next().unwrap().phase, phase);
                session.set_skip_reading(true, true, false);
                while vm.advance_time(&session, logical_time, 1).unwrap().pending {}
            } else {
                assert!(request.is_none());
            }

            session.set_skip_reading(false, false, false);
            if let Some(request) = request {
                assert!(
                    !vm.complete_transition_request(
                        request.id,
                        Ok((picture.clone(), saved.clone()))
                    )
                    .unwrap()
                );
            }
            drive(&mut vm, &mut session, false);
            while vm
                .advance_time(&session, logical_time.max(1.), 2)
                .unwrap()
                .pending
            {}
            drive(&mut vm, &mut session, false);
            let view = vm.transition_views().next().unwrap();
            assert_eq!(view.phase, TransitionPhase::FinalPending);
            assert!(view.previous.is_none() && view.mask.is_none());
            assert!(
                !vm.accept_transition(view.id, TransitionPhase::NewPending, 0., vm.logical_time())
                    .unwrap()
            );
            assert!(
                vm.stage()
                    .nodes()
                    .any(|node| node.kind == NodeKind::Text("new drawing".into()))
            );
            assert!(
                !vm.stage()
                    .nodes()
                    .any(|node| node.kind == NodeKind::Text("done".into()))
            );
            accept_final(&mut vm);
            assert!(
                !vm.accept_transition(
                    view.id,
                    TransitionPhase::FinalPending,
                    1.,
                    vm.logical_time()
                )
                .unwrap()
            );
            drive(&mut vm, &mut session, false);
            assert!(
                vm.stage()
                    .nodes()
                    .any(|node| node.kind == NodeKind::Text("done".into()))
            );
        }
    }
}

#[test]
fn transition_leaf_nodes_keep_identity_and_wait_for_both_presentations() {
    for encoded in [false, true] {
        for (make, change, check) in [
            (
                "image(\"old.png\",width:40,height:40)",
                "node.source=\"new.png\"",
                "node.source==\"new.png\"",
            ),
            (
                "text(\"before\")",
                "node.value=\"after\"",
                "node.value==\"after\"",
            ),
        ] {
            let code = format!(
                r#"fn opening() {{
                let node={make};stage.replace(stack(){{node;}});
                let original=node;
                stage.transition(target:node,duration:1,change:||{{{change};}});
                if node!=original || !({check}) {{throw "leaf identity";}}
                narrator("done");
            }}"#
            );
            let (mut session, mut vm) = prepared(&code, encoded);
            drive(&mut vm, &mut session, false);
            let request = vm.take_transition_request().unwrap().unwrap();
            assert!(request.target.is_some());
            vm.accept_presentation(&session, request.presentation_request)
                .unwrap();
            vm.complete_transition_request(request.id, Ok(retained_picture()))
                .unwrap();
            drive(&mut vm, &mut session, false);
            let view = vm.transition_views().next().unwrap();
            assert_eq!(view.phase, TransitionPhase::NewPending);
            vm.accept_transition(view.id, view.phase, 0., vm.logical_time())
                .unwrap();
            vm.advance_time(&session, 1., 64).unwrap();
            accept_final(&mut vm);
            drive(&mut vm, &mut session, false);
            assert!(matches!(session.view().interaction,
                Some(crate::InteractionView::Dialogue(d)) if d.text()=="done"));
        }
    }
}

fn restored(vm: &SessionVm, session: &Session) -> (Session, SessionVm) {
    let mut capture = vm.snapshot_begin(session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut image =
        snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        vm.vm.program().clone(),
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(41),
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

#[test]
fn transition_zero_duration_waits_for_accepted_frame_returns_identity_and_restores() {
    for encoded in [false, true] {
        let code = r#"fn opening() {
            let state={calls:0, result:{value:42}};
            stage.replace(stack() { text("before"); });
            let result=stage.transition(duration:0,change:|| {
                state.calls+=1;
                stage.replace(stack() { text("changed"); });
                state.result
            });
            if result!=state.result || state.calls!=1 { throw "change replay or result identity"; }
            stage.replace(text("finished"));
        }"#;
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        assert!(vm.take_transition_request().unwrap().is_none());
        let view = vm.transition_views().next().unwrap();
        assert_eq!(view.phase, TransitionPhase::FinalPending);
        assert!(view.previous.is_none());
        assert!(
            !vm.accept_transition(view.id, TransitionPhase::NewPending, 0., vm.logical_time())
                .unwrap()
        );
        assert!(
            !vm.accept_transition(view.id, view.phase, 0.99, vm.logical_time())
                .unwrap()
        );
        let (mut session, mut vm) = restored(&vm, &session);
        assert!(
            !vm.accept_transition(view.id, view.phase, 1., vm.logical_time())
                .unwrap(),
            "old session receipt"
        );
        accept_final(&mut vm);
        drive(&mut vm, &mut session, true);
        assert!(vm.transition_views().next().is_none());
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("finished".into()))
        );
    }
}

#[test]
fn elastic_transition_limits_coverage_without_finishing_at_an_overshoot() {
    for encoded in [false, true] {
        for (curve, time, coverage) in [("elastic_out", 0.1, 1.), ("elastic_in", 0.9, 0.)] {
            let code = format!(
                "fn opening() {{ stage.replace(stack()); stage.transition(duration:1,\
                 easing:\"{curve}\",change:|| stage.replace(text(\"new\"))); }}"
            );
            let (mut session, mut vm) = prepared(&code, encoded);
            drive(&mut vm, &mut session, false);
            let request = vm.take_transition_request().unwrap().unwrap();
            vm.complete_transition_request(request.id, Ok(retained_picture()))
                .unwrap();
            drive(&mut vm, &mut session, false);
            let view = vm.transition_views().next().unwrap();
            assert!(
                vm.accept_transition(view.id, TransitionPhase::NewPending, 0., vm.logical_time())
                    .unwrap()
            );
            vm.advance_time(&session, time, 64).unwrap();
            let view = vm.transition_views().next().unwrap();
            assert_eq!(view.progress, coverage);
            assert_eq!(view.phase, TransitionPhase::Playing);
            assert!(
                !vm.accept_transition(
                    view.id,
                    TransitionPhase::FinalPending,
                    1.,
                    vm.logical_time()
                )
                .unwrap()
            );
            vm.advance_time(&session, 1., 64).unwrap();
            accept_final(&mut vm);
            drive(&mut vm, &mut session, true);
        }
    }
}

#[test]
fn transition_change_awaits_on_original_main_task_and_restores_inside_callback() {
    for encoded in [false, true] {
        let code = r#"fn opening() {
            stage.replace(stack());
            let state={calls:0};
            let result=stage.transition(duration:0,change:|| {
                state.calls+=1;
                narrator("inside original task");
                await run(delay(0.1));
                17
            });
            if state.calls!=1 || result!=17 { throw "callback replay"; }
            stage.replace(text("continued"));
        }"#;
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        assert_eq!(
            vm.transition_views().next().unwrap().phase,
            TransitionPhase::Changing
        );
        let (mut session, mut vm) = restored(&vm, &session);
        let interaction = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(interaction))
                .unwrap();
        }
        drive(&mut vm, &mut session, false);
        while vm.advance_time(&session, 1., 64).unwrap().pending {}
        drive(&mut vm, &mut session, false);
        accept_final(&mut vm);
        drive(&mut vm, &mut session, true);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("continued".into()))
        );
    }
}

#[test]
fn transition_owner_retirement_unwinds_inner_scope_and_preserves_business_changes() {
    let code = r#"fn opening() {
        let owner=stack();
        let target=stack();
        stage.replace(column() { owner; target; });
        let ready=completion();
        let state={calls:0, inner:false, caught:false};
        let work=spawn(|| { await ready; owner.remove(); });
        try { stage.transition(target:target,owner:owner,duration:0,change:|| {
            state.calls+=1;
            target.add(text("current stage"));
            scope(body:|s| {
                s.add(target,text("temporary"));
                ready.resolve();
                try { await completion(); } finally { state.inner=true; }
            });
        }); } catch error {
            if error!="TransitionCancelled" { throw error; }
            state.caught=true;
        }
        await work;
        if !state.caught || state.calls!=1 || state.inner { throw "transition cancellation"; }
        target.add(text("outer continued"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        let texts: Vec<_> = vm
            .stage()
            .nodes()
            .filter_map(|node| {
                if let NodeKind::Text(text) = &node.kind {
                    Some(text.plain().as_ref())
                } else {
                    None
                }
            })
            .collect();
        assert!(texts.contains(&"current stage"));
        assert!(texts.contains(&"outer continued"));
        assert!(!texts.contains(&"temporary"));
        assert!(vm.transition_views().next().is_none());
        assert!(vm.scopes.live.is_empty());
    }
}

#[test]
fn transition_overlap_fault_and_independent_subtrees_follow_existing_tasks() {
    let code = r#"fn opening() {
        let a=stack(); let b=stack();
        stage.replace(column() { a; b; });
        let child=a.add(stack());
        let started=completion(); let release=completion();
        let state={rejected:0,calls:0};
        let task=spawn(|| stage.transition(target:a,duration:0,change:|| {
            state.calls+=1;
            started.resolve();
            await release;
            23
        }));
        await started;
        for target in [null,a,child] {
            try { stage.transition(target:target,duration:0,change:|| { state.calls+=100; }); }
            catch error { state.rejected+=1; }
        }
        let second=spawn(|| stage.transition(target:b,duration:0,change:|| { b.add(text("second")); }));
        release.resolve();
        let result=await task;
        if result!=23 { throw "spawn transition result"; }
        await second;
        if state.calls!=1 || state.rejected!=3 { throw "overlap executed change"; }
        stage.replace(text("parallel complete"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        assert_eq!(vm.transition_views().count(), 2);
        accept_final(&mut vm);
        drive(&mut vm, &mut session, true);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("parallel complete".into()))
        );
        assert!(vm.transition_views().next().is_none());
    }
}

#[test]
fn transition_overlap_respects_independent_popup_layers() {
    let code = r#"fn opening() {
        let body=stack();
        let menu=body.add(popup());
        let nested=menu.add(popup());
        let body_child=body.add(stack());
        let menu_child=menu.add(stack());
        let nested_child=nested.add(stack());
        stage.replace(body);
        let release=completion();
        let tasks=[];
        let state={rejected:0,calls:0};
        for target in [body,menu,nested] {
            let started=completion();
            tasks.append(spawn(|| stage.transition(target:target,duration:0,change:|| {
                state.calls+=1;
                started.resolve();
                await release;
                23
            })));
            await started;
        }
        for target in [null,body,menu,nested,body_child,menu_child,nested_child] {
            try { stage.transition(target:target,duration:0,change:|| { state.calls+=100; }); }
            catch error { state.rejected+=1; }
        }
        release.resolve();
        for task in tasks {
            if await task!=23 { throw "popup transition result"; }
        }
        if state.calls!=3 || state.rejected!=7 { throw "popup transition overlap"; }
        stage.replace(text("popup transitions complete"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        assert_eq!(vm.transition_views().count(), 3);
        accept_final(&mut vm);
        drive(&mut vm, &mut session, true);
        assert!(
            vm.stage()
                .nodes()
                .any(|node| { node.kind == NodeKind::Text("popup transitions complete".into()) })
        );
        assert!(vm.transition_views().next().is_none());
    }
}

#[test]
fn transition_callback_fault_identity_and_task_cancel_release_state() {
    let code = r#"fn opening() {
        stage.replace(stack());
        let original={reason:"original"};
        let mut same=false;
        try { stage.transition(duration:0,change:|| { throw original; }); }
        catch error { same=error==original; }
        if !same { throw "fault replaced"; }
        let ready=completion();
        let state={cleanup:false};
        let work=spawn(|| stage.transition(duration:0,change:|| {
            ready.resolve();
            try { await completion(); } finally { state.cleanup=true; }
        }));
        await ready; work.cancel();
        let mut cancelled=false;
        try { await work; } catch error { cancelled=true; }
        if !cancelled || state.cleanup { throw "task cancel"; }
        stage.replace(text("cancelled"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        assert!(vm.transition_views().next().is_none());
        assert!(vm.tasks.is_empty());
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("cancelled".into()))
        );
    }
}

#[test]
fn transition_retains_before_change_starts_after_zero_frame_and_restores_playback() {
    use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
    use reel_ui::frame::{BuildLimits, StepBudget};
    let code = r#"fn opening() {
        let state={calls:0};
        stage.replace(stack());
        let result=stage.transition(duration:1,easing:"quad_in",change:|| {
            state.calls+=1;
            stage.replace(stack() { text("new side"); });
            91
        });
        if result!=91 || state.calls!=1 { throw "change replay"; }
        stage.replace(text("finished"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        assert!(
            vm.snapshot_begin(&session).is_err(),
            "unfinished retention is a save barrier"
        );
        let request = vm.take_transition_request().unwrap().unwrap();
        assert!(request.presentation_request > 0);
        assert!(vm.take_transition_request().unwrap().is_none());
        assert!(
            !vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("new side".into()))
        );
        let (picture, saved) = retained_picture();
        vm.accept_presentation(&session, request.presentation_request)
            .unwrap();
        assert!(
            vm.complete_transition_request(request.id, Ok((picture.clone(), saved)))
                .unwrap()
        );
        assert!(
            !vm.complete_transition_request(request.id, Err("late delivery".into()))
                .unwrap()
        );
        drive(&mut vm, &mut session, false);
        let view = vm.transition_views().next().unwrap();
        assert_eq!(view.phase, TransitionPhase::NewPending);
        assert!(Arc::ptr_eq(view.previous.as_ref().unwrap(), &picture));
        vm.advance_time(&session, 4., 64).unwrap();
        assert_eq!(vm.transition_views().next().unwrap().progress, 0.);
        assert!(
            !vm.accept_transition(
                view.id,
                TransitionPhase::FinalPending,
                1.,
                vm.logical_time()
            )
            .unwrap()
        );
        assert!(
            vm.accept_transition(view.id, view.phase, 0., vm.logical_time())
                .unwrap()
        );
        vm.advance_time(&session, 4.5, 64).unwrap();
        assert_eq!(vm.transition_views().next().unwrap().progress, 0.25);
        let (mut session, mut vm) = restored(&vm, &session);
        let (id, saved) = vm.pending_transition_pictures().next().unwrap();
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 64,
                max_bytes: 64 << 20,
            },
        );
        let mut restore = saved
            .restore_begin(
                BuildLimits {
                    max_bytes: 16 << 20,
                },
                &pool,
            )
            .unwrap();
        let budget = StepBudget {
            max_commands: 64,
            max_units: 8192,
            max_bytes: 64 << 10,
        };
        loop {
            let progress = restore.step(budget).unwrap();
            assert!(progress.missing_asset.is_none());
            if progress.build.complete {
                break;
            }
        }
        vm.install_transition_picture(id, Arc::new(restore.finish().unwrap()))
            .unwrap();
        assert_eq!(vm.transition_views().next().unwrap().progress, 0.25);
        vm.advance_time(&session, 5., 64).unwrap();
        drive(&mut vm, &mut session, false);
        assert!(
            !vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("finished".into()))
        );
        accept_final(&mut vm);
        drive(&mut vm, &mut session, true);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("finished".into()))
        );
        assert!(vm.transition_views().next().is_none());
    }
}

#[test]
fn transition_retention_failure_and_prechange_cancel_do_not_run_callback() {
    for encoded in [false, true] {
        let code = r#"fn opening() {
            stage.replace(stack());
            let state={calls:0,caught:false};
            try { stage.transition(change:|| { state.calls+=1; }); }
            catch error { state.caught=true; }
            if state.calls!=0 || !state.caught { throw "failed retention executed change"; }
            stage.replace(text("failure handled"));
        }"#;
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        let request = vm.take_transition_request().unwrap().unwrap();
        assert!(
            vm.complete_transition_request(request.id, Err("retention budget".into()))
                .unwrap()
        );
        assert!(
            !vm.complete_transition_request(request.id, Err("duplicate".into()))
                .unwrap()
        );
        drive(&mut vm, &mut session, true);
        assert!(vm.transition_views().next().is_none());
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("failure handled".into()))
        );

        let code = r#"fn opening() {
            stage.replace(stack());
            let state={calls:0};
            let work=spawn(|| stage.transition(change:|| { state.calls+=1; }));
            await run(delay(0.01));
            work.cancel();
            try { await work; } catch error {}
            if state.calls!=0 { throw "cancelled retention executed change"; }
            stage.replace(text("cancelled before change"));
        }"#;
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        let request = vm.take_transition_request().unwrap().unwrap();
        vm.advance_time(&session, 1., 64).unwrap();
        drive(&mut vm, &mut session, true);
        assert!(!vm.transition_request_pending(request.id).unwrap());
        assert!(vm.transition_views().next().is_none());
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("cancelled before change".into()))
        );
    }
}

#[test]
fn transition_target_retirement_cancels_callback_and_keeps_outer_task() {
    let code = r#"fn opening() {
        let target=stack();
        let root=column() { target; };
        stage.replace(root);
        let ready=completion();
        let state={changed:false,caught:false,inner:false};
        let remover=spawn(|| { await ready; target.remove(); });
        try { stage.transition(target:target,duration:0,change:|| {
            state.changed=true;
            ready.resolve();
            try { await completion(); } catch error { state.inner=true; }
        }); } catch error {
            if error!="TransitionCancelled" { throw error; }
            state.caught=true;
        }
        await remover;
        if !state.changed || !state.caught || state.inner { throw "target cancellation"; }
        root.add(text("outer task survived"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        assert!(vm.transition_views().next().is_none());
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("outer task survived".into()))
        );
    }
}

#[test]
fn transition_rejects_unmounted_and_cursor_targets_before_change() {
    let code = r#"fn opening() {
        let leaf=text("leaf");
        let pointer_child=stack();
        let pointer=cursor() { pointer_child; };
        stage.replace(column() { leaf; pointer; });
        let detached=stack();
        let state={calls:0,errors:0};
        for target in [detached,pointer,pointer_child] {
            try { stage.transition(target:target,duration:0,change:|| { state.calls+=1; }); }
            catch error { state.errors+=1; }
        }
        if state.calls!=0 || state.errors!=3 { throw "invalid transition target"; }
        stage.replace(text("targets validated"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        assert!(vm.transition_views().next().is_none());
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("targets validated".into()))
        );
    }
}

#[test]
fn transition_parallel_clock_maintenance_respects_work_and_publication_barriers() {
    let code = r#"fn opening() {
        let root=column(); stage.replace(root);
        let work=[];
        let mut i=0;
        while i<130 {
            let target=root.add(stack());
            work.append(spawn(|| stage.transition(target:target,duration:1,change:|| {})));
            i+=1;
        }
        for task in work { await task; }
        stage.replace(text("all complete"));
    }"#;
    let (mut session, mut vm) = prepared(code, true);
    drive(&mut vm, &mut session, false);
    let (picture, saved) = retained_picture();
    let mut count = 0;
    while let Some(request) = vm.take_transition_request().unwrap() {
        vm.complete_transition_request(request.id, Ok((picture.clone(), saved.clone())))
            .unwrap();
        count += 1;
    }
    assert_eq!(count, 130);
    drive(&mut vm, &mut session, false);
    let views: Vec<_> = vm.transition_views().collect();
    for view in views {
        assert_eq!(view.phase, TransitionPhase::NewPending);
        assert!(
            vm.accept_transition(view.id, view.phase, 0., vm.logical_time())
                .unwrap()
        );
    }
    let progress = vm.advance_time(&session, 0.5, 1).unwrap();
    assert!(progress.work <= 1);
    assert!(progress.pending);
    assert!(!vm.stage_ready());
    assert!(vm.snapshot_begin(&session).is_err());
    let mut work = progress.work;
    for _ in 0..200 {
        let progress = vm.advance_time(&session, 0.5, 1).unwrap();
        assert!(progress.work <= 1);
        work += progress.work;
        if !progress.pending {
            break;
        }
    }
    assert_eq!(work, 130);
    assert!(vm.stage_ready());
    assert!(vm.transition_views().all(|view| view.progress == 0.5));
    while vm.advance_time(&session, 1., 17).unwrap().pending {}
    accept_final(&mut vm);
    drive(&mut vm, &mut session, true);
    assert!(vm.transition_views().next().is_none());
    assert!(
        vm.stage()
            .nodes()
            .any(|node| node.kind == NodeKind::Text("all complete".into()))
    );
}

#[test]
fn transition_rejects_tiled_mask_before_change_or_retention() {
    let code = r#"fn opening() {
        stage.replace(stack());
        let state={calls:0,errors:0};
        for source in ["large.reel-image", ""] {
            try { stage.transition(mask:source,change:|| { state.calls+=1; }); }
            catch error { state.errors+=1; }
            try { stage.transition(mask:mask.alpha(source),change:|| { state.calls+=1; }); }
            catch error { state.errors+=1; }
        }
        if state.calls!=0 || state.errors!=4 { throw "invalid transition mask"; }
        stage.replace(text("masks validated"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        assert!(vm.take_transition_request().unwrap().is_none());
        assert!(vm.transition_views().next().is_none());
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("masks validated".into()))
        );
    }
}

#[test]
fn transition_finished_continuation_restores_after_its_target_is_retired() {
    let code = r#"fn opening() {
        let root=column(); let target=root.add(stack()); stage.replace(root);
        let state={changes:0,result:{value:42}};
        let result=stage.transition(target:target,duration:0.1,change:|| {
            state.changes+=1; state.result
        });
        if result!=state.result || state.changes!=1 { throw "completed change replay"; }
        root.add(text("returned after retirement"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        let request = vm.take_transition_request().unwrap().unwrap();
        let (picture, saved) = retained_picture();
        vm.complete_transition_request(request.id, Ok((picture, saved)))
            .unwrap();
        drive(&mut vm, &mut session, false);
        let view = vm.transition_views().next().unwrap();
        vm.accept_transition(view.id, TransitionPhase::NewPending, 0., vm.logical_time())
            .unwrap();
        while vm.advance_time(&session, 0.1, 64).unwrap().pending {}
        accept_final(&mut vm);
        // This is the same retirement path as a node.remove host call from
        // another task, before the completed transition's native return runs.
        let target = vm.stage.value(vm.vm.heap(), view.target.unwrap()).unwrap();
        vm.stage.remove_node(vm.vm.heap_mut(), target).unwrap();
        assert!(vm.transition_views().next().is_none());
        let (mut session, mut vm) = restored(&vm, &session);
        assert_eq!(vm.pending_transition_pictures().count(), 0);
        drive(&mut vm, &mut session, true);
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("returned after retirement".into()))
        );
    }
}

#[test]
fn transition_retired_target_drains_cancellation_before_snapshot() {
    let code = r#"fn opening() {
        let root=column(); let target=root.add(stack()); stage.replace(root);
        let state={changes:0,caught:false};
        try { stage.transition(target:target,duration:0,change:|| {
            state.changes+=1; await completion();
        }); } catch error {
            if error!="TransitionCancelled" { throw error; }
            state.caught=true;
        }
        if state.changes!=1 || !state.caught { throw "lost target cancellation"; }
        root.add(text("cancelled before snapshot"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        let view = vm.transition_views().next().unwrap();
        assert_eq!(view.phase, TransitionPhase::Changing);
        let target = vm.stage.value(vm.vm.heap(), view.target.unwrap()).unwrap();
        vm.stage.remove_node(vm.vm.heap_mut(), target).unwrap();
        assert!(vm.snapshot_begin(&session).is_err());
        drive(&mut vm, &mut session, true);
        let (mut session, mut vm) = restored(&vm, &session);
        drive(&mut vm, &mut session, true);
        assert!(vm.transition_views().next().is_none());
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("cancelled before snapshot".into()))
        );
    }
}

#[test]
fn transition_snapshot_accounts_retained_picture_before_vm_capture_with_one_unit_steps() {
    let code = r#"fn opening() {
        stage.replace(stack());
        stage.transition(duration:1,change:|| { await completion(); });
    }"#;
    let (mut session, mut vm) = prepared(code, true);
    drive(&mut vm, &mut session, false);
    let request = vm.take_transition_request().unwrap().unwrap();
    let (picture, saved) = retained_picture();
    let retained_bytes: u64 = saved.retained_size_steps().sum();
    let size_steps = saved.retained_size_steps().count() + 2;
    assert!(retained_bytes >= picture.accounted_bytes());
    vm.complete_transition_request(request.id, Ok((picture, saved)))
        .unwrap();
    drive(&mut vm, &mut session, false);

    let mut capture = vm.snapshot_begin(&session).unwrap();
    assert_eq!(capture.step(SnapshotBudget { units: 0 }).unwrap().bytes, 0);
    let mut measured = 0;
    for _ in 0..size_steps {
        let progress = capture.step(SnapshotBudget { units: 1 }).unwrap();
        assert_eq!(progress.units, 1);
        assert!(!progress.complete);
        measured += progress.bytes;
    }
    // This is the byte count passed to the host's bounded save queue. It must
    // include the picture held by the save, even before any VM records exist.
    assert_eq!(
        measured,
        retained_bytes + size_of::<Option<Arc<reel_ui::frame::SavedRetainedScene>>>() as u64
    );
}

#[test]
fn transition_lifetimes_are_rechecked_after_retirement_and_not_vm_reentry() {
    let (mut session, mut vm) = prepared(
        r#"fn opening() {
        let node=stack(); stage.replace(node);
        stage.transition(target:node, duration:1, change:||{});
    }"#,
        false,
    );
    drive(&mut vm, &mut session, false);
    assert!(vm.transition_views().next().is_some());
    vm.maintain_transitions(&mut session).unwrap();
    let before = vm.transitions.lifetime_checks.get();
    for _ in 0..4096 {
        assert!(!vm.transitions_pending());
        vm.maintain_transitions(&mut session).unwrap();
    }
    assert_eq!(vm.transitions.lifetime_checks.get(), before);
    println!(
        "transition_lifetimes reentries=4096 owner_checks={}",
        vm.transitions.lifetime_checks.get() - before
    );
    let target = vm.stage().root().unwrap();
    let value = vm.stage.value(vm.vm.heap(), target).unwrap();
    vm.stage.remove_node(vm.vm.heap_mut(), value).unwrap();
    assert!(vm.transitions_pending());
    vm.maintain_transitions(&mut session).unwrap();
    assert!(vm.transition_views().next().is_none());
}
