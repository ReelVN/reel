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
fn class_models_preserve_types_methods_aliases_and_control_transactions() {
    let code = r#"
    let constructed = {count: 0};
    class Base {
        let title: string = "title";
        let mut name: string = "initial";
        let mut score: float = 2.0;
        let items = [];
        fn init(self) { constructed.count += 1; }
        fn caption(self) -> string { f"{self.name}:{self.items.len()}" }
    }
    class Child: Base {
        let mut friend: Base|null = null;
        let mut pair: (int,string) = (1,"a");
        fn change(self, event) {
            self.name = "updated";
            self.score = 4.0;
            self.items.append(1);
        }
    }
    let escaped = {value: null};
    class Broken {
        let value: int;
        fn init(self) { escaped.value = self; throw "construction failed"; }
    }
    class Constructing {
        let mut value: int = 1;
        fn init(self) {
            let mut refused = false;
            try { model(self); } catch error { refused = true; }
            if !refused { throw "unfinished instance was copied"; }
        }
    }
    fn opening() {
        Constructing();
        try { Broken(); } catch error {}
        let mut refused = false;
        try { model(escaped.value); } catch error { refused = true; }
        if !refused { throw "failed instance was copied"; }
        let original = Child();
        original.friend = original;
        let data = model({first: original, second: original, items: [original]});
        if !(data.first is Child) || !(data.first is Base) || constructed.count != 1
            || data.first != data.second || data.items[0] != data.first
            || data.first.friend != data.first || data.first == original { throw "class copy"; }
        data.first.name = "copy";
        if data.second.name != "copy" || original.name != "initial" { throw "copy alias"; }
        let mut rejected = 0;
        try { bind(original.score); } catch error { rejected += 1; }
        try { data.first.title = "changed"; } catch error { rejected += 1; }
        try { text_input(bind(data.first.title)); } catch error { rejected += 1; }
        try { model({method: data.first.caption}); } catch error { rejected += 1; }
        try { bind(data.first[42]); } catch error { rejected += 1; }
        try { bind(data.first.caption); } catch error { rejected += 1; }
        try { bind(data.first["missing"]); } catch error { rejected += 1; }
        if rejected != 7 { throw "class model boundaries"; }
        let field_name = "na" + "me";
        let view = text(bind(data.first[field_name]), x: bind(data.first.score));
        let control = slider(value: bind(data.first.score), min: 0, max: 10);
        control.value = 8.0;
        if data.first.score != 8.0 || view.x != 8.0 { throw "typed writeback"; }
        rejected = 0;
        try { data.first.score = 11.0; } catch error { rejected += 1; }
        try { data.first.score = "invalid"; } catch error { rejected += 1; }
        if rejected != 2 || data.first.score != 8.0 || view.x != 8.0 || control.value != 8.0 { throw "field transaction"; }
        let loose = model({friend: null, pair: (1,"a")});
        data.first.friend = bind(loose.friend);
        data.first.pair = bind(loose.pair);
        rejected = 0;
        try { loose.friend = 42; } catch error { rejected += 1; }
        try { loose.pair = (1,2); } catch error { rejected += 1; }
        if rejected != 2 || loose.friend != null || data.first.friend != null
            || loose.pair[1] != "a" || data.first.pair[1] != "a" { throw "full field type contracts"; }
        let external = Child();
        loose.friend = external;
        let nested = text(bind(external.name));
        external.name = "adopted";
        if nested.value != "adopted" || data.first.friend != external { throw "adopted instance"; }
        data.first.name = "initial";
        stage.replace(column {
            text(bind(data.first.title));
            control; view; nested;
            text(derive(|| data.first.caption()));
            button("change", on_click: data.first.change);
        });
        narrator("class model ready");
    }
    "#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        assert!(drive(&mut vm, &mut session, true) > 1);
        collect(&mut vm);
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("initial:0".into()))
        );
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
                    range: Some(reel_ui::widgets::SliderRange::new(0., 10., None).unwrap()),
                }
            )
            .unwrap()
        );
        drive(&mut vm, &mut session, false);
        assert_eq!(
            vm.stage()
                .nodes()
                .find(|node| node.kind == NodeKind::Text("initial".into()))
                .unwrap()
                .props
                .x,
            6.
        );
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
                .any(|node| node.kind == NodeKind::Text("updated:1".into()))
        );
        assert_eq!(
            vm.stage()
                .nodes()
                .find(|node| node.kind == NodeKind::Text("updated".into()))
                .unwrap()
                .props
                .x,
            4.
        );
        vm.revoke(&mut session).unwrap();
    }
}

#[test]
fn cancelling_a_large_model_copy_releases_its_work_graph() {
    let (mut session, mut vm) = setup(
        r#"class Entry {
            let index: int;
            fn init(self, index: int) { self.index = index; }
        }
        fn opening() {
        let values = [];
        for i in 0..256 { values.append(Entry(i)); }
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
