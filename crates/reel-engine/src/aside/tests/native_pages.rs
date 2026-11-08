use super::{
    models::{drive, prepared},
    *,
};
use reel_ui::stage::{NodeKind, NodeSemantics};

#[test]
fn native_page_workers_are_registered_without_author_exports() {
    for symbol in ["reel::page_stack::produce", "reel::tab_bar::observe_label"] {
        assert!(
            source::contracts()
                .iter()
                .any(|contract| contract.symbol == symbol)
        );
        assert!(
            !source::documentation()
                .iter()
                .any(|entry| entry.contract.symbol == symbol)
        );
        assert!(
            !source::api()
                .functions
                .iter()
                .any(|entry| aside_host::source::canonical(&entry.symbol) == symbol)
        );
        let code = format!("use {symbol}; fn main(){{}}");
        assert!(source::compile_module(&code, "private", "private.aside", 1).is_err());
    }
    for field in ["key", "label", "selected", "control"] {
        assert!(
            source::api()
                .member("reel::types::TabItem", field)
                .unwrap()
                .write_type
                .is_none()
        );
        let code = format!(
            "use reel::types::TabItem; fn change(item:TabItem){{item.{field}=item.{field};}}"
        );
        assert!(source::compile_module(&code, "readonly", "readonly.aside", 1).is_err());
    }
}

fn click(vm: &mut SessionVm, session: &mut Session, label: &str) {
    let id = vm
        .stage()
        .nodes()
        .find_map(|n| match &n.kind {
            NodeKind::Button { label: Some(v), .. } if v == label => Some(n.id),
            _ => None,
        })
        .unwrap();
    assert!(vm.click(session, id).unwrap());
    drive(vm, session, false);
}
fn text_id(vm: &SessionVm, label: &str) -> reel_ui::stage::NodeId {
    vm.stage()
        .nodes()
        .find(|n| n.kind == NodeKind::Text(label.into()))
        .unwrap()
        .id
}

#[test]
fn pages_retain_binding_identity_and_inactive_eviction_through_restore() {
    let code = r#"fn opening() {
        let data=model({key:"one",shown:true,count:0});
        let pages=page_stack(value:bind(data.key),items:[
            {key:"one",build:|| {data.count+=1; text("one",visible:bind(data.shown))}},
            {key:"two",build:|| {data.count+=1; text("two")}}
        ],width:300,height:200);
        stage.replace(column {
            pages;
            button("two",on_click:|| {data.key="two";});
            button("one",on_click:|| {data.key="one";});
            button("hide",on_click:|| {data.shown=false;});
            button("evict",on_click:|| {if !pages.evict("two") {throw "missing cached page";}});
            button("check",on_click:|| {if data.count!=3 {throw "build count";} if pages.get("two")==null {throw "missing second";}});
        });
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        assert!(vm.stage_ready());
        assert!(
            !vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("two".into()))
        );
        let first = text_id(&vm, "one");
        click(&mut vm, &mut session, "two");
        let second = text_id(&vm, "two");
        click(&mut vm, &mut session, "one");
        assert_eq!(text_id(&vm, "one"), first);
        assert_eq!(text_id(&vm, "two"), second);
        click(&mut vm, &mut session, "hide");
        assert!(!vm.stage().node(first).unwrap().props.visible);
        (session, vm) = scopes::restored(&mut vm, &session);
        drive(&mut vm, &mut session, true);
        click(&mut vm, &mut session, "evict");
        assert!(vm.stage().node(second).is_none());
        click(&mut vm, &mut session, "two");
        assert_ne!(text_id(&vm, "two"), second);
        click(&mut vm, &mut session, "check");
    }
}

#[test]
fn ensure_shares_task_retry_failure_cancel_and_consumer_cancellation() {
    let code = r#"fn opening() {
        let data={built:0,fail:true};
        let pages=page_stack(items:[{key:"page",build:|| {
            data.built+=1;
            if data.fail {throw "original failure";}
            text("page")
        }}]);
        stage.replace(pages);
        if pages.get("page")!=null || pages.evict("page") {throw "eager build";}
        let failed=pages.ensure("page");
        if failed!=pages.ensure("page") {throw "producer identity";}
        let mut failures=0;
        try {await failed;} catch e {failures+=1;}
        try {await failed;} catch e {failures+=1;}
        if failures!=2 || data.built!=1 {throw "shared failure";}
        data.fail=false;
        let cancelled=pages.ensure("page");
        if !cancelled.cancel() {throw "explicit cancel";}
        if pages.ensure("page")!=cancelled {throw "cancelling producer was not shared";}
        try {await cancelled;} catch e {failures+=1;}
        if failures!=3 || cancelled.pending {throw "cancellation did not settle";}
        let task=pages.ensure("page");
        let waiter=spawn(|| {await task;throw "cancelled consumer resumed";});
        let other=spawn(|| {await task;});
        waiter.cancel();
        try {await waiter;} catch e {failures+=1;}
        let node=await task;
        await other;
        if task.pending || pages.get("page")!=node || await pages.ensure("page")!=node {throw "ready cache";}
        if data.built!=2 {throw "producer ran more than once";}
        let mut rejected=false;
        pages.value="page";
        try {pages.evict("page");} catch e {rejected=true;}
        if !rejected {throw "selected eviction accepted";}
    }"#;
    for encoded in [false, true] {
        let (mut s, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut s, true);
        assert_eq!(
            vm.stage()
                .nodes()
                .filter(|n| n.kind == NodeKind::Text("page".into()))
                .count(),
            1
        );
        assert!(vm.stage_ready());
    }
}

#[test]
fn tabs_have_independent_groups_and_observable_custom_content() {
    let code = r#"fn opening() {
        let data=model({key:"one",label:"One"});
        let options=[{key:"one",label:bind(data.label)},{key:"two",label:"Two"}];
        let first=tab_bar(value:bind(data.key),items:options,
            build:|item| text(bind(item.label),opacity:derive(|| if item.selected {1} else {0.25})));
        let second=tab_bar(value:bind(data.key),items:options,orientation:"vertical",activation:"manual");
        stage.replace(column {first;second;button("select",on_click:|| {data.key="two";});button("label",on_click:|| {data.label="Changed";});});
    }"#;
    for encoded in [false, true] {
        let (mut s, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut s, true);
        let groups = vm
            .stage()
            .nodes()
            .filter_map(|n| match n.kind {
                NodeKind::Radio { group, tabs, .. } => {
                    assert!(tabs.is_some());
                    assert_eq!(n.props.semantics, NodeSemantics::Tab);
                    group
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(groups.len(), 4);
        assert_eq!(groups[0], groups[1]);
        assert_eq!(groups[2], groups[3]);
        assert_ne!(groups[0], groups[2]);
        assert_eq!(
            vm.stage().node(text_id(&vm, "One")).unwrap().props.opacity,
            1.
        );
        click(&mut vm, &mut s, "select");
        assert_eq!(
            vm.stage().node(text_id(&vm, "One")).unwrap().props.opacity,
            0.25
        );
        assert_eq!(
            vm.stage().node(text_id(&vm, "Two")).unwrap().props.opacity,
            1.
        );
        click(&mut vm, &mut s, "label");
        assert_eq!(
            vm.stage()
                .node(text_id(&vm, "Changed"))
                .unwrap()
                .props
                .opacity,
            0.25
        );
        (s, vm) = scopes::restored(&mut vm, &s);
        drive(&mut vm, &mut s, true);
        assert_eq!(
            vm.stage()
                .nodes()
                .filter(|n| n.props.semantics == NodeSemantics::TabList)
                .count(),
            2
        );
    }
}

#[test]
fn selecting_a_large_tab_bar_publishes_only_the_two_changed_item_records() {
    let code = r#"fn opening() {
        let data=model({key:0}); let count={built:0}; let items=[];
        for i in 0..384 {items.append({key:i,label:f"tab {i}"});}
        let bar=tab_bar(value:bind(data.key),items:items,build:|item| {count.built+=1;text(item.label)});
        stage.replace(column {bar;button("select",on_click:|| {data.key=197;if count.built!=384 {throw "rebuilt content";}});});
    }"#;
    let (mut s, mut vm) = prepared(code, true);
    drive(&mut vm, &mut s, true);
    let bar = vm
        .stage()
        .nodes()
        .find(|n| n.props.semantics == NodeSemantics::TabList)
        .unwrap()
        .id;
    let heap = vm.vm.heap();
    let host = vm.stage.value(heap, bar).unwrap();
    let state = heap
        .derived_state(heap.host_payload(host, 3).unwrap())
        .unwrap();
    let entries = heap.sequence_get(state, 2).unwrap();
    let records = (0..384)
        .map(|i| {
            let e = heap.sequence_get(entries, i).unwrap();
            let row = heap.sequence_get(e, 3).unwrap();
            let fields = heap.host_fields(row).unwrap();
            (fields, heap.info(fields).unwrap().content_version)
        })
        .collect::<Vec<_>>();
    let nodes = vm
        .stage()
        .nodes()
        .filter(|n| matches!(n.kind, NodeKind::Text(_)))
        .map(|n| n.id)
        .collect::<Vec<_>>();
    click(&mut vm, &mut s, "select");
    for (i, (fields, version)) in records.into_iter().enumerate() {
        assert_eq!(
            vm.vm.heap().info(fields).unwrap().content_version != version,
            i == 0 || i == 197,
            "row {i}"
        );
    }
    assert!(nodes.into_iter().all(|id| vm.stage().node(id).is_some()));
}

#[test]
fn factories_reject_external_waits_and_stack_retirement_settles_producers() {
    let code = r#"fn opening() {
        let owner=stack();stage.replace(owner);
        let pages=page_stack(items:[{key:"page",build:|| {await completion();text("bad")}}]);
        owner.add(pages);
        let task=pages.ensure("page");let mut caught=false;
        try {await task;} catch e {caught=true;}
        if !caught || task.pending || pages.get("page")!=null {throw "waiting factory did not fail";}
        let invalid=page_stack(items:[{key:"bad",build:|| 42}]);owner.add(invalid);
        caught=false;
        try {await invalid.ensure("bad");} catch e {
            if e.code!="Type" || !e.details.starts_with("model:") {throw "invalid factory result lost constructor location";}
            caught=true;
        }
        if !caught {throw "non-Node factory result was accepted";}
        let doomed=page_stack(items:[{key:"page",build:|| {throw "retired factory ran";}}]);
        owner.add(doomed);
        let pending=doomed.ensure("page");doomed.remove();
        caught=false;try {await pending;} catch e {caught=true;}
        if !caught || pending.pending {throw "owner cancellation did not settle";}
    }"#;
    for encoded in [false, true] {
        let (mut s, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut s, true);
        assert!(vm.stage_ready());
        assert!(vm.tasks.is_empty());
    }
}

#[test]
fn task_admission_failure_leaves_ensure_retryable() {
    let code = r#"fn opening() {
        let pages=page_stack(items:[{key:"page",build:|| text("admitted")}]);
        stage.replace(pages);
        let occupied=spawn(|| {await completion();});
        let mut rejected=false;
        try {pages.ensure("page");} catch e {rejected=true;}
        if !rejected {throw "task quota was not exercised";}
        occupied.cancel();try {await occupied;} catch e {}
        let retry=pages.ensure("page");
        let node=await retry;
        if pages.get("page")!=node || retry.pending {throw "failed admission poisoned cache";}
    }"#;
    let mut s = session();
    let story = source::compile_story(code, "admission", 1, "opening").unwrap();
    let initializer = story.initializer;
    let entry = story.entry;
    let mut vm = SessionVm::load_story(
        &s,
        story,
        VmLimits {
            max_tasks: 2,
            ..Default::default()
        },
    )
    .unwrap();
    vm.start(&s, initializer).unwrap();
    drive(&mut vm, &mut s, true);
    vm.start(&s, entry).unwrap();
    drive(&mut vm, &mut s, true);
    assert!(
        vm.stage()
            .nodes()
            .any(|n| n.kind == NodeKind::Text("admitted".into()))
    );
}

#[test]
fn page_factory_continuation_survives_snapshot_and_gc() {
    let code = r#"fn opening() {
        stage.replace(page_stack(value:"page",items:[{key:"page",build:|| {
            let mut total=0;for i in 0..256 {total+=i;}
            text(f"restored {total}")
        }}]));
    }"#;
    let (mut s, mut vm) = prepared(code, true);
    let mut found = false;
    for _ in 0..10_000 {
        match next(&mut vm, &mut s) {
            RunOutcome::HostCall(t) => {
                let p = vm.vm.program().package();
                let producer = p.symbols[p.natives[t.native.index()].symbol.index()]
                    == "reel::page_stack::produce"
                    && t.input == NativeInput::Entry;
                vm.dispatch(&mut s, t, "en", NativeBudget::RetryMinimum(8))
                    .unwrap();
                if producer {
                    found = true;
                    break;
                }
            }
            RunOutcome::Returned(_) => vm.discard_delivery(&mut s).unwrap(),
            RunOutcome::Fault(id) => panic!("{:?}", vm.vm.delivery_fault(id).unwrap()),
            _ => {}
        }
    }
    assert!(found);
    assert!(
        !vm.stage_ready(),
        "the selected page is still being constructed"
    );
    collect(&mut vm);
    (s, vm) = scopes::restored(&mut vm, &s);
    drive(&mut vm, &mut s, true);
    assert!(vm.stage_ready());
    assert!(
        vm.stage()
            .nodes()
            .any(|n| n.kind == NodeKind::Text("restored 32640".into()))
    );
}

#[test]
fn copied_specs_reject_structural_mutation_between_native_slices() {
    let (mut s, mut vm) = prepared(
        r#"fn opening(){let _=page_stack(items:[{key:1,build:|| text("one")},{key:2,build:|| text("two")}]);}"#,
        false,
    );
    let mut changed = false;
    for _ in 0..10_000 {
        match next(&mut vm, &mut s) {
            RunOutcome::HostCall(t) => {
                let p = vm.vm.program().package();
                let page =
                    p.symbols[p.natives[t.native.index()].symbol.index()] == "reel::page_stack";
                if page && t.continuation.is_some_and(|c| c.stage == 0) {
                    let source = vm.vm.host_arguments(t).unwrap()[1];
                    let item = vm.vm.heap().sequence_get(source, 0).unwrap();
                    vm.vm.heap_mut().array_push(source, item).unwrap();
                    assert!(matches!(
                        vm.dispatch(&mut s, t, "en", NativeBudget::Exact(1)),
                        Err(AdapterError::Heap(HeapError::ConcurrentModification))
                    ));
                    changed = true;
                    break;
                }
                vm.dispatch(&mut s, t, "en", NativeBudget::RetryMinimum(8))
                    .unwrap();
            }
            RunOutcome::Returned(_) => vm.discard_delivery(&mut s).unwrap(),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(changed);
    vm.revoke(&mut s).unwrap();
}

#[test]
fn common_page_properties_override_restore_and_interaction_use_the_declared_prefix() {
    use reel_ui::stage::{InteractionState, NodeInteraction};
    let code = r#"fn opening() {
        let look=appearance(hover:{opacity:0.45});
        let pages=page_stack(value:"page",items:[{key:"page",build:|| text("page")}],width:321,opacity:0.8,appearance:look);
        let tabs=tab_bar(value:bind(pages.value),items:[{key:"page",label:"Page"}],width:400,opacity:0.9,appearance:look);
        stage.replace(column{pages;tabs;});
        scope(body:|s| {
            s.override(pages,{opacity:0.2,visible:false});
            s.override(tabs,{opacity:0.3,visible:false});
            narrator("covered",history:false);
        });
        if pages.opacity!=0.8 || !pages.visible || tabs.opacity!=0.9 || !tabs.visible {throw "scope did not restore common fields";}
        pages.x=42;tabs.y=63;
        narrator("ordinary",history:false);
    }"#;
    for encoded in [false, true] {
        let (mut s, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut s, true);
        let page = vm
            .stage()
            .nodes()
            .find(|n| n.props.width == Some(321.))
            .unwrap()
            .id;
        let tab = vm
            .stage()
            .nodes()
            .find(|n| n.props.semantics == NodeSemantics::TabList)
            .unwrap()
            .id;
        assert_eq!(
            (
                vm.stage().node(page).unwrap().props.opacity,
                vm.stage().node(tab).unwrap().props.opacity
            ),
            (0.2, 0.3)
        );
        assert!(!vm.stage().node(page).unwrap().props.visible);
        assert!(!vm.stage().node(tab).unwrap().props.visible);
        (s, vm) = scopes::restored(&mut vm, &s);
        drive(&mut vm, &mut s, true);
        let interaction = s.view().interaction.unwrap().id();
        for _ in 0..2 {
            s.handle_local_input(s.root_owner(), InputAction::Advance(interaction))
                .unwrap();
        }
        drive(&mut vm, &mut s, true);
        let page_props = &vm.stage().node(page).unwrap().props;
        let tab_props = &vm.stage().node(tab).unwrap().props;
        assert_eq!(
            (page_props.opacity, page_props.x, page_props.visible),
            (0.8, 42., true)
        );
        assert_eq!(
            (tab_props.opacity, tab_props.y, tab_props.visible),
            (0.9, 63., true)
        );
        for node in [page, tab] {
            vm.stage
                .interaction(
                    vm.vm.heap_mut(),
                    &NodeInteraction {
                        node,
                        state: InteractionState {
                            hover: true,
                            ..Default::default()
                        },
                    },
                )
                .unwrap();
        }
        vm.sync_stage(&s).unwrap();
        assert_eq!(vm.stage().node(page).unwrap().props.opacity, 0.45);
        assert_eq!(vm.stage().node(tab).unwrap().props.opacity, 0.45);
        assert_eq!(vm.stage().node(page).unwrap().props.x, 42.);
        assert_eq!(vm.stage().node(tab).unwrap().props.y, 63.);
    }
}

#[test]
fn object_page_keys_keep_lookup_ensure_and_selection_after_restore() {
    let code = r#"fn opening() {
        let first={id:"first"};let second={id:"second"};let third={id:"third"};
        let pages=page_stack(value:first,items:[
            {key:third,build:|| text("third")},
            {key:first,build:|| text("first")},
            {key:second,build:|| text("second")}
        ]);
        let tabs=tab_bar(value:bind(pages.value),items:[{key:second,label:"Second"},{key:third,label:"Third"},{key:first,label:"First"}]);
        stage.replace(column {pages;tabs;button("verify",on_click:|| {
            for key in [third,first,second] {
                let cached=pages.get(key);if cached==null {throw "object key lost";}
                if await pages.ensure(key)!=cached {throw "object page rebuilt";}
                pages.value=key;
            }
        });});
        for key in [third,first,second] {await pages.ensure(key);}
    }"#;
    for encoded in [false, true] {
        let (mut s, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut s, true);
        let ids = [
            text_id(&vm, "first"),
            text_id(&vm, "second"),
            text_id(&vm, "third"),
        ];
        (s, vm) = scopes::restored(&mut vm, &s);
        drive(&mut vm, &mut s, true);
        click(&mut vm, &mut s, "verify");
        assert_eq!(
            ids,
            [
                text_id(&vm, "first"),
                text_id(&vm, "second"),
                text_id(&vm, "third")
            ]
        );
        assert!(vm.stage_ready());
    }
}

#[test]
fn tab_content_uses_the_same_native_control_for_feedback_appearance_gc_and_restore() {
    use reel_ui::stage::{InteractionState, NodeInteraction};
    let code = r#"fn opening() {
        let saved={item:null,control:null,builds:0};
        let bar=tab_bar(value:0,activation:"manual",items:[{key:0,label:"First"},{key:1,label:"Second"}],build:|item| {
            saved.builds+=1;
            if item.key==0 {saved.item=item;saved.control=item.control;}
            item.control.appearance=appearance(hover:{opacity:0.6});
            row {
                text(bind(item.label));
                text(f"marker {item.key}",visible:bind(item.control.focus_visible),
                    opacity:derive(|| if item.control.focus_visible {1} else {0.2}));
            }
        });
        stage.replace(column {bar;button("identity",on_click:|| {
            if saved.item.control!=saved.control || saved.builds!=2 {throw "control identity or factory changed";}
        });});
    }"#;
    for encoded in [false, true] {
        let (mut s, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut s, true);
        let control = vm
            .stage()
            .nodes()
            .find(|n| matches!(&n.kind,NodeKind::Radio{label:Some(label),..} if label=="First"))
            .unwrap()
            .id;
        let marker = text_id(&vm, "marker 0");
        assert!(!vm.stage().node(marker).unwrap().props.visible);
        for restored in [false, true] {
            if restored {
                collect(&mut vm);
                (s, vm) = scopes::restored(&mut vm, &s);
                drive(&mut vm, &mut s, true);
                assert!(
                    !vm.stage().node(marker).unwrap().props.visible,
                    "restoration resets transient focus feedback"
                );
            }
            vm.stage
                .interaction(
                    vm.vm.heap_mut(),
                    &NodeInteraction {
                        node: control,
                        state: InteractionState {
                            hover: true,
                            focused: true,
                            focus_visible: true,
                            ..Default::default()
                        },
                    },
                )
                .unwrap();
            drive(&mut vm, &mut s, true);
            assert_eq!(vm.stage().node(control).unwrap().props.opacity, 0.6);
            assert!(vm.stage().node(marker).unwrap().props.visible);
            assert_eq!(vm.stage().node(marker).unwrap().props.opacity, 1.);
            click(&mut vm, &mut s, "identity");
        }
    }
}

#[test]
fn failed_tab_factories_retire_exposed_controls_without_restarting() {
    let code = r#"fn opening() {
        stage.replace(stack());
        let saved={control:null,built:0};let selected=model({value:0});let original={message:"factory fault"};
        for mode in [0,1,2] {
            selected.value=0;let mut caught=false;
            try {
                let _=tab_bar(value:bind(selected.value),items:[{key:0,label:"First"},{key:1,label:"Second"}],build:|item| {
                    saved.built+=1;saved.control=item.control;
                    if mode==0 {throw original;}
                    if mode==1 {return 42;}
                    item.control.remove();text("detached")
                });
            } catch e {
                if mode==0 && e!=original {throw "factory fault identity";}
                caught=true;
            }
            if !caught {throw "invalid content factory completed";}
            let mut retired=false;try {let _=saved.control.visible;} catch e {retired=true;}
            if !retired {throw "failed factory left control live";}
            selected.value=1;await run(delay(0));
            if saved.built!=mode+1 {throw "failed factory restarted";}
        }
    }"#;
    for encoded in [false, true] {
        let (mut s, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut s, true);
        collect(&mut vm);
        vm.sync_stage(&s).unwrap();
        assert!(
            !vm.stage()
                .nodes()
                .any(|node| matches!(node.kind, NodeKind::Radio { .. }))
        );
    }
}

#[test]
fn evicting_the_previous_projection_after_selection_does_not_leave_a_retired_wrapper() {
    let code = r#"fn opening() {
        let pages=page_stack(value:"one",items:[{key:"one",build:|| text("one")},{key:"two",build:|| text("two")}]);
        stage.replace(column {pages;button("switch",on_click:|| {
            pages.value="two";if !pages.evict("one") {throw "previous page was not cached";}
        });});
    }"#;
    for encoded in [false, true] {
        let (mut s, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut s, true);
        let old = text_id(&vm, "one");
        click(&mut vm, &mut s, "switch");
        assert!(vm.stage().node(old).is_none());
        assert!(vm.stage().node(text_id(&vm, "two")).is_some());
        assert!(vm.stage_ready());
    }
}
