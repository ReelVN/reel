use super::*;

#[test]
fn composed_actions_share_children_and_preserve_paused_repeated_playback_after_restore() {
    let source = r#"fn opening() {
        let node = stack(); stage.replace(node);
        let entrance = tween(node, from: {x: 0}, x: 10, duration: 0.5);
        let passage = sequence(entrance, delay(0.5),
            tween(node, from: {y: 0}, y: 20, duration: 0.5));
        let fading = tween(node, from: {opacity: 1}, opacity: 0.5, duration: 1.5);
        let motion = repeat(parallel(passage, fading), times: 2).run(paused: true);
        narrator("prepared");
        if motion.resume().pause().resume() != motion { throw "control identity"; }
        if await motion != "Completed" || node.x != 10 || node.y != 20 {
            throw "repeated passage";
        }
        let start = entrance.run;
        let short = start(paused: true);
        if short.finish() != short || await short != "Completed" {
            throw "retained method and reusable action";
        }
        let pending = delay(9).run();
        if await pending.stop() != "Stopped" { throw "stop result"; }
        let timer = clock(interval: 0.5);
        if timer.pause().resume().stop() != timer || await timer != "Stopped" {
            throw "clock controls";
        }
        narrator("done");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        models::drive(&mut vm, &mut session, false);
        advance_dialogue(&mut vm, &mut session);
        let allocations = vm.vm.heap().stats().chunk_allocations;
        for tick in 1..=45 {
            advance_clock(&mut vm, &session, tick as f64 / 60., 1);
        }
        assert_eq!(vm.vm.heap().stats().chunk_allocations, allocations);
        vm.sync_stage(&session).unwrap();
        let root = vm.stage().root().unwrap();
        let props = &vm.stage().node(root).unwrap().props;
        assert_eq!((props.x, props.y), (10., 0.));
        assert_eq!(props.opacity, 0.75);

        let (mut session, mut vm) = scopes::restored(&mut vm, &session);
        advance_clock(&mut vm, &session, 1.25, 1);
        vm.sync_stage(&session).unwrap();
        let root = vm.stage().root().unwrap();
        assert_eq!(vm.stage().node(root).unwrap().props.y, 10.);
        advance_clock(&mut vm, &session, 1.75, 1);
        vm.sync_stage(&session).unwrap();
        assert_eq!(vm.stage().node(root).unwrap().props.x, 5.);
        advance_clock(&mut vm, &session, 3., 1);
        models::drive(&mut vm, &mut session, false);
        assert!(matches!(session.view().interaction,
            Some(crate::InteractionView::Dialogue(d)) if d.text() == "done"));
        assert!(!vm.activities_need_time());
    }
}

#[test]
fn composed_actions_keep_scope_ownership_and_reject_conflicts_before_writing() {
    let source = r#"fn dynamic(value) { value }
    fn opening() {
        let node = stack(x: 3); stage.replace(node);
        let mut rejected = false;
        try {
            parallel(tween(node, x: 10), tween(node, x: 20)).run();
        } catch error { rejected = true; }
        if !rejected || node.x != 3 { throw "parallel conflict"; }
        rejected = false;
        try { sequence(delay(1), dynamic(0)); } catch error { rejected = true; }
        if !rejected { throw "action type"; }
        let mut scoped = null;
        scope(body: || { scoped = sequence(delay(1), delay(2)).run(); });
        let mut cancelled = false;
        try { await scoped; } catch error { cancelled = true; }
        if !cancelled { throw "scope ownership"; }
        let owned = delay(5).run(owner: node, paused: true);
        node.remove();
        cancelled = false;
        try { await owned; } catch error { cancelled = true; }
        if !cancelled { throw "node ownership"; }
        narrator("done");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        models::drive(&mut vm, &mut session, false);
        assert!(matches!(session.view().interaction,
            Some(crate::InteractionView::Dialogue(d)) if d.text() == "done"));
        assert!(!vm.activities_need_time());
    }
}
