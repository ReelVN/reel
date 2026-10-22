//! Real pages exercise moved reading data and the same retained observable models.
use super::*;
use crate::{InputOutcome, InteractionView};
use reel_ui::stage::NodeKind;

fn advance(vm: &mut SessionVm, session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        if session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap()
            == InputOutcome::Completed
        {
            break;
        }
    }
    models::drive(vm, session, true);
}

fn click(vm: &mut SessionVm, session: &mut Session, label: &str) {
    let id = vm
        .stage()
        .nodes()
        .find_map(|n| match &n.kind {
            NodeKind::Button {
                label: Some(text), ..
            } if text == label => Some(n.id),
            _ => None,
        })
        .unwrap();
    vm.click(session, id).unwrap();
    models::drive(vm, session, true);
}

fn has_text(vm: &SessionVm, text: &str) -> bool {
    vm.stage()
        .nodes()
        .any(|n| n.kind == NodeKind::Text(text.into()))
}

#[test]
fn reading_nested_page_restores_original_models_open_speech_and_shared_delay_source_asb() {
    let code = r#"use reel::types::{Reading,Scope};
    fn opening() {
        let parent:Reading=game.reading().set_auto(true,delay:3,condition:false).pause(true);
        let dialogue=game.dialogue();let choice=game.choice();let history=game.history();
        let line=narrator(rich("parent"),wait:false);
        let saved={child:null};
        let result=reading(body:|page:Scope| {
            let child:Reading=game.reading();saved.child=child;
            if child==parent || child.auto || child.paused || child.skipping || !child.auto_ready
                || child.delay!=3 || game.history().len()!=0 || game.dialogue().active {
                throw "fresh reading";
            }
            if parent.pause(false).set_auto(delay:5)!=parent || child.paused || child.auto
                || child.delay!=5 || parent.paused || !parent.auto || parent.auto_ready {
                throw "receiver escaped its context";
            }
            scope(body:|| {
                if game.reading()!=child || game.history()!=game.history() {throw "scope copied reading";}
            });
            narrator("child checkpoint");
            42
        });
        if result!=42 || game.reading()!=parent || game.dialogue()!=dialogue
            || game.choice()!=choice || game.history()!=history || history.len()!=1
            || dialogue.text.plain!="parent" || dialogue.content_id==null
            || !parent.auto || parent.paused || parent.delay!=5 || parent.auto_ready {
            throw "parent was rebuilt or polluted";
        }
        let mut retired=0;
        try {saved.child.pause(false);}catch error{retired+=1;}
        if retired!=1 || saved.child.paused || saved.child.auto {throw "retired reading changed final values";}
        line.wait();
        if history.len()!=1 || history[0].text.plain!="parent" {throw "history replaced";}
        stage.replace(text("reading complete"));
    }"#;
    for encoded in [false, true] {
        for restored in [false, true] {
            let (mut session, mut vm) = models::prepared(code, encoded);
            models::drive(&mut vm, &mut session, true);
            assert!(matches!(session.view().interaction,
                Some(InteractionView::Dialogue(d)) if d.text()=="child checkpoint"));
            assert_eq!(session.parked_readings.len(), 1);
            if restored {
                (session, vm) = scopes::restored(&mut vm, &session);
            }
            advance(&mut vm, &mut session);
            assert!(matches!(session.view().interaction,
                Some(InteractionView::Dialogue(d)) if d.text()=="parent"));
            assert!(session.parked_readings.is_empty());
            assert_eq!(session.read_progress().ids().count(), 1);
            advance(&mut vm, &mut session);
            assert!(
                vm.stage()
                    .nodes()
                    .any(|n| n.kind == NodeKind::Text("reading complete".into()))
            );
            assert_eq!(session.read_progress().ids().count(), 2);
            assert_eq!(session.view().history.len(), 1);
            assert!(vm.narrative_contexts.frames.is_empty());
        }
    }
}

#[test]
fn reading_event_finish_restores_parent_bindings_and_retires_paused_child_source_asb() {
    let code = r#"fn opening() {
        let parent=game.reading();let history=game.history();
        let root=column() {
            text(derive(||if parent.paused {"parent paused"}else{"parent ready"}));
            text(derive(||if game.reading().paused {"active paused"}else{"active ready"}));
        };stage.replace(root);
        let state={child:null,dialogue:null};
        let result=reading(body:|page| {
            let controls=game.reading();state.child=controls;
            state.dialogue=game.dialogue();
            let voice=model({pending:true});
            controls.set_auto(true,delay:4,condition:derive(||!voice.pending)).pause(true);
            page.add(root,column() {
                button("parent pause",on_click:|e|parent.pause(true));
                button("parent resume",on_click:|e|parent.pause(false));
                button("leave",on_click:|e|page.finish("title"));
            });
            narrator("abandoned child");throw "finished story continued";
        });
        if result!="title" || game.reading()!=parent || parent.paused || parent.auto
            || parent.skipping || !parent.auto_ready || parent.delay!=4 || game.history()!=history
            || history.len()!=0 || game.dialogue().active {throw "child contaminated parent";}
        let mut retired=0;
        try {state.child.set_auto();}catch error{retired+=1;}
        try {state.dialogue.clear();}catch error{retired+=1;}
        if retired!=2 {throw "child models remained live";}
        if state.child.paused || state.child.auto || state.child.skipping || !state.child.auto_ready
            || state.dialogue.active || state.dialogue.ready || state.dialogue.advance_requested
            || state.dialogue.text!="abandoned child" {throw "retired terminal state";}
        root.add(text("event returned"));
        narrator("parent checkpoint");
    }"#;
    for encoded in [false, true] {
        for restored in [false, true] {
            let (mut session, mut vm) = models::prepared(code, encoded);
            models::drive(&mut vm, &mut session, true);
            assert!(session.reading().paused());
            assert!(!session.reading().auto_ready());
            assert!(has_text(&vm, "parent ready"));
            assert!(has_text(&vm, "active paused"));
            if restored {
                (session, vm) = scopes::restored(&mut vm, &session);
            }
            let condition = vm
                .narrative
                .as_ref()
                .unwrap()
                .condition_weak(vm.vm.heap())
                .unwrap();
            click(&mut vm, &mut session, "parent pause");
            assert!(has_text(&vm, "parent paused"));
            assert!(has_text(&vm, "active paused"));
            click(&mut vm, &mut session, "parent resume");
            assert!(has_text(&vm, "parent ready"));
            assert!(
                session.reading().paused(),
                "parent receiver changed foreground"
            );
            let old = session.view().interaction.unwrap().id();
            click(&mut vm, &mut session, "leave");
            assert!(has_text(&vm, "event returned"));
            assert!(has_text(&vm, "active ready"));
            assert!(has_text(&vm, "parent ready"));
            assert!(
                matches!(session.view().interaction,Some(InteractionView::Dialogue(d))
                if d.text()=="parent checkpoint" && d.id()!=old)
            );
            assert!(!session.reading().automatic());
            assert!(!session.reading().paused());
            assert!(session.reading().auto_ready());
            assert!(
                !vm.stage()
                    .nodes()
                    .any(|n| matches!(n.kind, NodeKind::Button { .. }))
            );
            assert!(session.read_progress().ids().next().is_none());
            collect(&mut vm);
            assert!(
                vm.vm.heap_mut().upgrade(condition).unwrap().is_none(),
                "the retired auto condition retained the old scene closure"
            );
            advance(&mut vm, &mut session);
        }
    }
}

#[test]
fn reading_accepts_completed_main_awaits_without_confusing_scheduler_wait_with_interaction() {
    let code = r#"fn opening() {
        let worker=spawn(||17);
        if await worker!=17 {throw "task result";}
        if reading(body:||41)!=41 {throw "reading after task await";}
        let value=scope(body:|| {
            let ready=completion();spawn(||ready.resolve(29));await ready
        });
        if value!=29 || reading(body:||43)!=43 {throw "reading after scope completion";}
        stage.replace(text("await then reading"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        assert!(has_text(&vm, "await then reading"));
        assert!(vm.narrative_contexts.frames.is_empty());
        assert!(session.parked_readings.is_empty());
    }
}

#[test]
fn reading_nested_exit_paths_restore_parent_without_replaying_body_source_asb() {
    for exit in ["return", "throw", "cancel", "finish"] {
        let operation = match exit {
            "return" => "return 17;",
            "throw" => "throw 17;",
            "cancel" => "outer.cancel();await completion();",
            "finish" => "outer.finish(expected);await completion();",
            _ => unreachable!(),
        };
        let code = format!(
            r#"fn opening() {{
            let root=stack();stage.replace(root);
            let parent=game.reading().set_auto(true,condition:false);
            let history=game.history();game.record("parent");
            let expected={{destination:"title"}};
            let state={{runs:0,cleaned:false,cancel:null,finish:null}};
            let mut result=null;let mut fault=null;
            try {{result=reading(body:|outer| {{
                state.cancel=outer.cancel;state.finish=outer.finish;
                outer.add(root,text("outer"));
                game.reading().pause(true);game.record("outer");
                reading(body:|inner| {{
                    inner.add(root,text("inner"));
                    game.reading().set_skip(true,unread:true);game.record("inner");
                    state.runs+=1;
                    try {{ {operation} }} finally {{state.cleaned=true;}}
                }})
            }});}}catch error{{fault=error;}}
            if state.runs!=1 || state.cleaned!=({normal}) || state.cancel() || state.finish(null)
                || game.reading()!=parent || !parent.auto || parent.paused || parent.skipping
                || parent.auto_ready || game.history()!=history || history.len()!=1
                || history[0].text!="parent" {{throw "nested reading cleanup";}}
            if "{exit}"=="return" && (result!=17 || fault!=null) {{throw "normal return";}}
            if "{exit}"=="throw" && fault!=17 {{throw "exception return";}}
            if "{exit}"=="cancel" && fault!="ScopeCancelled" {{throw "cancel boundary";}}
            if "{exit}"=="finish" && (result!=expected || fault!=null) {{throw "finish identity";}}
            root.add(text("nested complete"));
        }}"#,
            normal = matches!(exit, "return" | "throw")
        );
        for encoded in [false, true] {
            let (mut session, mut vm) = models::prepared(&code, encoded);
            models::drive(&mut vm, &mut session, true);
            assert!(has_text(&vm, "nested complete"), "{exit}");
            assert_eq!(vm.stage().nodes().count(), 2, "{exit}");
            assert!(vm.narrative_contexts.frames.is_empty());
            assert!(session.parked_readings.is_empty());
        }
    }
}

#[test]
fn reading_rejects_non_story_tasks_and_releases_contexts_on_session_revocation() {
    let code = r#"fn opening() {
        let parent=game.reading();let original=game.history();
        let failed=spawn(|| {
            try {reading(body:||{throw "callback entered";});}catch error{return;}
            throw "callback reading accepted";
        });await failed;
        if game.reading()!=parent || game.history()!=original {throw "failed entry changed parent";}
        reading(body:|| {
            game.reading().set_auto(true,condition:false).pause(true);
            reading(body:||{game.reading();narrator("stop session");});
        });
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        assert_eq!(session.parked_readings.len(), 2);
        vm.revoke(&mut session).unwrap();
        assert!(session.parked_readings.is_empty());
        assert!(vm.narrative_contexts.frames.is_empty());
        assert!(vm.pending.is_none());
    }
}

#[test]
fn reading_outer_finish_request_restores_with_nested_wait_and_original_result_identity() {
    let code = r#"fn opening() {
        let parent=game.reading();let history=game.history();
        let root=column();stage.replace(root);
        let expected={page:"title"};let state={runs:0};
        let result=reading(body:|outer| {
            game.reading().set_auto(true,condition:false).pause(true);
            outer.add(root,button("leave nested",on_click:||outer.finish(expected)));
            reading(body:|| {
                state.runs+=1;game.reading().set_auto(delay:7);
                narrator("nested checkpoint");throw "child body continued";
            });
            throw "parent body continued";
        });
        if result!=expected || state.runs!=1 || game.reading()!=parent || game.history()!=history
            || history.len()!=0 || parent.paused || parent.auto || !parent.auto_ready
            || parent.delay!=7 {throw "restored reading exit";}
        root.add(text("saved exit"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        (session, vm) = scopes::restored(&mut vm, &session);
        let button = vm
            .stage()
            .nodes()
            .find_map(|n| match &n.kind {
                NodeKind::Button {
                    label: Some(label), ..
                } if label == "leave nested" => Some(n.id),
                _ => None,
            })
            .unwrap();
        vm.click(&mut session, button).unwrap();
        for _ in 0..10_000 {
            if vm
                .scopes
                .live
                .iter()
                .any(|s| matches!(s.exit, Some(crate::aside::scopes::ScopeExit::Finish)))
            {
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
                SessionStep::Vm(RunOutcome::Fault(id)) => {
                    panic!("before exit capture: {:?}", vm.vm.delivery_fault(id))
                }
                _ => {}
            }
        }
        assert!(
            vm.scopes
                .live
                .iter()
                .any(|s| matches!(s.exit, Some(crate::aside::scopes::ScopeExit::Finish)))
        );
        assert_eq!(session.parked_readings.len(), 2);
        collect(&mut vm);
        (session, vm) = scopes::restored(&mut vm, &session);
        models::drive(&mut vm, &mut session, true);
        assert!(has_text(&vm, "saved exit"));
        assert_eq!(vm.stage().nodes().count(), 2);
        assert!(session.view().interaction.is_none());
        assert!(session.parked_readings.is_empty());
        assert!(vm.narrative_contexts.frames.is_empty());
    }
}

#[test]
#[ignore = "reproducible reading-page navigation, retained history and idle storage measurement"]
fn reading_context_storage_cost() {
    let pages = std::env::var("REEL_READING_PAGES")
        .map(|s| s.parse::<usize>().unwrap())
        .unwrap_or(100);
    let history = std::env::var("REEL_READING_HISTORY")
        .map(|s| s.parse::<usize>().unwrap())
        .unwrap_or(1000);
    let code = format!(
        r#"fn opening() {{
        let parent=game.reading();let retained=game.history();
        let root=column(){{text(derive(||if game.reading().paused{{"paused"}}else{{"ready"}}));}};
        stage.replace(root);
        for i in 0..{history} {{game.record(f"parent {{i}}");}}
        for visit in 0..{pages} {{
            let destination=reading(body:|page| {{
                let voice=model({{pending:true}});
                game.reading().set_auto(true,condition:derive(||!voice.pending)).pause(true);
                page.add(root,button("leave",on_click:||page.finish("title")));
                narrator("page text");throw "retired page resumed";
            }});
            if destination!="title" || game.history()!=retained || retained.len()!={history}
                || game.reading()!=parent {{throw "reading lifetime";}}
        }}
        narrator("finished",history:false);
    }}"#
    );
    let (mut session, mut vm) = models::prepared(&code, true);
    models::drive(&mut vm, &mut session, false);
    collect(&mut vm);
    let mounted = vm.vm.heap().stats();
    let parent_capacity = session.parked_readings[0].data.history.capacity();
    let parent_pointer = session.parked_readings[0].data.history.as_ptr();
    let stable_capacity = session.storage_usage();
    for _ in 0..120 {
        vm.sync_stage(&session).unwrap();
        vm.sync_reading(&session).unwrap();
    }
    let idle = vm.vm.heap().stats();
    assert_eq!(idle.chunk_allocations, mounted.chunk_allocations);
    assert_eq!(session.storage_usage(), stable_capacity);
    let started = std::time::Instant::now();
    for visit in 0..pages {
        let id = vm
            .stage()
            .nodes()
            .find_map(|n| match &n.kind {
                NodeKind::Button {
                    label: Some(label), ..
                } if label == "leave" => Some(n.id),
                _ => None,
            })
            .unwrap();
        vm.click(&mut session, id).unwrap();
        // Collection is measured at explicit checkpoints, rather than forced
        // after every model-copy slice by the behavioral test helper.
        models::drive(&mut vm, &mut session, false);
        if visit + 1 < pages {
            assert_eq!(session.parked_readings.len(), 1);
            assert_eq!(
                session.parked_readings[0].data.history.as_ptr(),
                parent_pointer,
                "entering a reading page copied parent history"
            );
        }
        // Use the runtime's collection request policy at each settled page.
        if vm.vm.heap().collection_due() {
            collect(&mut vm);
        }
    }
    let elapsed = started.elapsed();
    collect(&mut vm);
    let retired = vm.vm.heap().stats();
    assert!(
        matches!(session.view().interaction,Some(InteractionView::Dialogue(d)) if d.text()=="finished")
    );
    assert!(session.parked_readings.is_empty());
    assert!(vm.narrative_contexts.frames.is_empty());
    assert_eq!(session.reading_data.history.as_ptr(), parent_pointer);
    assert_eq!(session.reading_data.history.capacity(), parent_capacity);
    assert_eq!(
        session.storage_usage().reserved_history_slots,
        parent_capacity
    );
    assert!(
        retired.live_objects <= mounted.live_objects,
        "objects leaked across {pages} pages"
    );
    assert!(
        retired.roots < mounted.roots,
        "reading/page roots survived retirement"
    );
    println!(
        "reading,pages={pages},history={history},navigation_ns={},mounted_bytes={},retired_bytes={},mounted_objects={},retired_objects={},mounted_roots={},retired_roots={},parent_history_capacity={parent_capacity},idle_chunks={}",
        elapsed.as_nanos(),
        mounted.charged_bytes,
        retired.charged_bytes,
        mounted.live_objects,
        retired.live_objects,
        mounted.roots,
        retired.roots,
        idle.chunk_allocations - mounted.chunk_allocations
    );
}
