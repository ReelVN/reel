use super::*;
use reel_ui::stage::{ControlValue, NodeChange, NodeKind};

pub(super) fn prepared(code: &str, encoded: bool) -> (Session, SessionVm) {
    let mut session = session();
    let mut story = source::compile_story(code, "model", 1, "opening").unwrap();
    if encoded {
        let bytes = bc::encode(story.program.package(), bc::Limits::default()).unwrap();
        story.program = bc::verify(
            bc::decode(&bytes, bc::Limits::default()).unwrap(),
            bc::Limits::default(),
        )
        .unwrap();
    }
    let mut vm = SessionVm::load(&session, story.program, VmLimits::default()).unwrap();
    vm.start(&session, story.initializer).unwrap();
    drive(&mut vm, &mut session, true);
    vm.start(&session, story.entry).unwrap();
    (session, vm)
}

pub(super) fn drive(vm: &mut SessionVm, session: &mut Session, collect_copies: bool) -> usize {
    let mut copies = 0;
    for _ in 0..100_000 {
        match vm
            .step(
                session,
                SliceBudget {
                    instructions: 7,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                let p = vm.vm.program().package();
                let model =
                    p.symbols[p.natives[ticket.native.index()].symbol.index()] == "reel::model";
                if model {
                    copies += 1;
                }
                if let Err(error) = vm.dispatch(session, ticket, "en", if model { 1 } else { 32 }) {
                    vm.reject_native(session, ticket, error.native_fault_kind().unwrap())
                        .unwrap();
                }
                if model && collect_copies {
                    collect(vm);
                }
            }
            SessionStep::Vm(RunOutcome::Fault(id)) => panic!(
                "model source fault: {:?}",
                vm.vm.delivery_fault(id).unwrap()
            ),
            SessionStep::Vm(RunOutcome::ResourceBlocked(error)) => {
                panic!("model resource: {error:?}")
            }
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(session).unwrap(),
            SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_))
                if !vm.has_runnable_tasks() =>
            {
                vm.sync_stage(session).unwrap();
                return copies;
            }
            _ => {}
        }
    }
    panic!("model task failed to settle");
}

#[test]
fn model_graphs_bind_controls_and_views_from_source_and_asb() {
    let code = r#"let initialized = model({score: 2});
    fn opening() {
        if initialized.score != 2 { throw "module model initialization"; }
        let shared = {score: 2, caption: "initial"};
        let mut ordinary = false;
        try { bind(shared.score); } catch e { ordinary = true; }
        if !ordinary { throw "ordinary table became observable"; }
        let defaults = {first: shared, second: shared, items: [shared], pair: (shared,)};
        defaults.self = defaults;
        let data = model(defaults);
        data.first.score = 3;
        if data.second.score != 3 || data.items[0].score != 3 || data.pair[0].score != 3
            || data.self.first.score != 3 || shared.score != 2 { throw "copy graph"; }
        let external = {score: 4};
        data.extra = external;
        let nested = text("nested", x: bind(data.extra.score));
        external.score = 5;
        if nested.x != 5 { throw "adopted alias"; }
        let inserted = {score: 6};
        data.items.append(inserted);
        let item = text("item", x: bind(inserted.score));
        inserted.score = 7;
        if item.x != 7 { throw "array adoption"; }
        data.link = bind(data.first.score);
        data.link = 4;
        if data.first.score != 4 { throw "new bound field"; }
        let control = slider(value: bind(data.first.score), min: 0, max: 10);
        let view = text(bind(data.first.caption), x: bind(data.second.score));
        control.value = 8;
        if data.first.score != 8 || view.x != 8 { throw "writeback"; }
        let mut rejected = false;
        try { data.first.score = 11; } catch e { rejected = true; }
        if !rejected || data.first.score != 8 || view.x != 8 || control.value != 8 { throw "atomic validation"; }
        let saved = bind(data.extra.score);
        data.extra.clear();
        data.extra.score = 9;
        if nested.x != 5 { throw "clear must freeze subscribers"; }
        let mut stale = false;
        try { nested.x = saved; } catch e { stale = true; }
        if !stale || nested.x != 5 { throw "stale property retargeted"; }
        nested.x = bind(data.extra.score);
        external.score = 10;
        if nested.x != 10 { throw "new property"; }
        let mut invalid = false;
        try { model({action: || 1}); } catch e { invalid = true; }
        if !invalid { throw "executable copied as data"; }
        stage.replace(column() { control; view; nested; item;
            button("rename", on_click: |event| { data.first.caption = "updated"; });
        });
        narrator("model ready");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        assert!(
            drive(&mut vm, &mut session, true) > 1,
            "copy must yield between work units"
        );
        collect(&mut vm);
        let slider = vm
            .stage()
            .nodes()
            .find(|node| matches!(node.kind, NodeKind::Slider { .. }))
            .unwrap()
            .id;
        assert!(
            vm.change(
                &mut session,
                &NodeChange {
                    node: slider,
                    value: ControlValue::Number(6.),
                    range: Some(reel_ui::widgets::SliderRange::new(0., 10., None).unwrap())
                }
            )
            .unwrap()
        );
        drive(&mut vm, &mut session, false);
        let view = vm
            .stage()
            .nodes()
            .find(|node| node.kind == NodeKind::Text("initial".into()))
            .unwrap();
        assert_eq!(view.props.x, 6.);
        let button = vm
            .stage()
            .nodes()
            .find(|node| matches!(node.kind, NodeKind::Button { .. }))
            .unwrap()
            .id;
        assert!(vm.click(&mut session, button).unwrap());
        drive(&mut vm, &mut session, false);
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("updated".into()))
        );
    }
}

#[test]
fn cancelling_a_large_model_copy_releases_its_work_graph() {
    let (mut session, mut vm) = setup(
        r#"fn opening() {
        let values = [];
        for i in 0..256 { values.append({index: i}); }
        let data = model({values: values});
        throw "cancelled model returned";
    }"#,
        Context::Ordinary,
    );
    let RunOutcome::HostCall(ticket) = next(&mut vm, &mut session) else {
        panic!("expected the model native call");
    };
    vm.dispatch(&mut session, ticket, "en", 16).unwrap();
    collect(&mut vm);
    let before = vm.vm.heap().stats();
    vm.revoke(&mut session).unwrap();
    while vm.cleanup_pending() {
        vm.cleanup_step(&mut session, 1, SliceBudget::default(), || 0)
            .unwrap();
    }
    collect(&mut vm);
    let after = vm.vm.heap().stats();
    assert!(after.live_objects + 256 < before.live_objects);
    assert!(after.charged_bytes < before.charged_bytes);
}
