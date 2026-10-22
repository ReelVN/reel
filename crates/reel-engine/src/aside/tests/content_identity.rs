use super::*;
use crate::{InputOutcome, InteractionView};

fn prepared(code: &str, mode: usize) -> (Session, SessionVm) {
    let mut session = session();
    let mut story = source::compile_story(code, "identity", 7, "opening").unwrap();
    if mode != 0 {
        let mut package = story.program.package().clone();
        if mode == 2 {
            for function in &mut package.functions {
                function.source_map.clear();
            }
            package.sources.clear();
        }
        story.program = bc::verify(
            bc::decode(
                &bc::encode(&package, bc::Limits::default()).unwrap(),
                bc::Limits::default(),
            )
            .unwrap(),
            bc::Limits::default(),
        )
        .unwrap();
    }
    let initializer = story.initializer;
    let entry = story.entry;
    let mut vm = SessionVm::load_story(&session, story, VmLimits::default()).unwrap();
    vm.start(&session, initializer).unwrap();
    models::drive(&mut vm, &mut session, false);
    vm.start(&session, entry).unwrap();
    (session, vm)
}

#[derive(Debug, PartialEq, Eq)]
struct Observation {
    ids: Vec<String>,
    text: String,
    read: bool,
}

fn observe(session: &Session) -> Observation {
    let Some(InteractionView::Dialogue(dialogue)) = session.view().interaction else {
        panic!("dialogue")
    };
    Observation {
        ids: dialogue
            .lines()
            .iter()
            .map(|line| line.content_id().to_owned())
            .collect(),
        text: dialogue.text().to_owned(),
        read: dialogue.previously_read(),
    }
}

fn advance(session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        if session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap()
            == InputOutcome::Completed
        {
            return;
        }
    }
    panic!("dialogue did not advance");
}

fn play(code: &str, mode: usize, checkpoints: usize) -> Vec<Observation> {
    let (mut session, mut vm) = prepared(code, mode);
    let mut observations = Vec::new();
    for _ in 0..checkpoints {
        models::drive(&mut vm, &mut session, true);
        assert!(vm.pending.is_some());
        observations.push(observe(&session));
        advance(&mut session);
    }
    models::drive(&mut vm, &mut session, true);
    assert!(vm.task().is_none());
    observations
}

#[test]
fn complete_author_paths_distinguish_nested_methods_and_native_callback_bridges_source_asb() {
    let code = r#"
        fn inner(value) {narrator(value);}
        fn outer(value) {inner(value);}
        class Voice {fn speak(self,value) {outer(value);}}
        fn opening() {
            narrator("direct");
            outer("first");outer("second");
            let voice=Voice();voice.speak("method first");voice.speak("method second");
            for i in 0..2 {outer("repeat");}
            let body=||outer("callback");
            scope(body:body);scope(body:body);
        }
    "#;
    let source = play(code, 0, 9);
    for mode in [1, 2] {
        assert_eq!(play(code, mode, 9), source, "ASB mode {mode}");
    }
    let ids: Vec<_> = source.iter().map(|o| &o.ids[0]).collect();
    assert_ne!(ids[1], ids[2]);
    assert_ne!(ids[3], ids[4]);
    assert_eq!(ids[5], ids[6]);
    assert_ne!(ids[7], ids[8]);
    assert_eq!(
        source.iter().map(|o| o.read).collect::<Vec<_>>(),
        [false, false, false, false, false, false, true, false, false]
    );
    let unique: std::collections::HashSet<_> = ids.into_iter().collect();
    assert_eq!(unique.len(), 8);
}

#[test]
fn character_group_and_append_share_call_paths_and_keep_distinct_read_checkpoints() {
    let code = r#"
        fn speak(who,value) {who(value);}
        fn group(who,value) {say([who.line(value),who.line("other")]);}
        fn append(line,value) {line.append(value,wait:false);}
        fn opening() {
            let who=character("Speaker");
            speak(who,"character first");speak(who,"character second");
            let line=narrator("base",wait:false);
            append(line,"A");line.wait();
            append(line,"B");line.wait();
            for i in 0..2 {append(line,"C");line.wait();}
            group(who,"group first");group(who,"group second");
            for i in 0..2 {group(who,"group repeat");}
        }
    "#;
    let source = play(code, 0, 10);
    for mode in [1, 2] {
        assert_eq!(play(code, mode, 10), source);
    }
    assert_ne!(source[0].ids, source[1].ids);
    assert_eq!(source[2].ids, source[3].ids);
    assert_eq!(source[3].ids, source[4].ids);
    assert_eq!(source[4].ids, source[5].ids);
    assert_eq!(source[5].text, "baseABCC");
    assert!(!source[3].read, "a different append caller is unread");
    assert!(!source[4].read, "the loop append site is initially unread");
    assert!(
        source[5].read,
        "the same append site repeats its checkpoint"
    );
    assert_eq!(source[6].ids.len(), 2);
    assert_ne!(source[6].ids[0], source[6].ids[1]);
    assert_ne!(source[6].ids, source[7].ids);
    assert_eq!(source[8].ids, source[9].ids);
    assert!(!source[8].read);
    assert!(source[9].read);
}

#[test]
fn locale_and_dynamic_parameters_do_not_change_a_repeated_logical_site() {
    let code = r#"fn opening() {
        for locale in ["en","fr"] {
            localization.set_locale(locale);
            narrator(tr(f"source {locale}",{en:f"English {locale}",fr:f"Français {locale}"}));
        }
    }"#;
    let source = play(code, 0, 2);
    for mode in [1, 2] {
        assert_eq!(play(code, mode, 2), source);
    }
    assert_eq!(source[0].ids, source[1].ids);
    assert_ne!(source[0].text, source[1].text);
    assert!(!source[0].read);
    assert!(source[1].read);
}

#[test]
fn exact_identity_budget_is_admitted_before_any_publication_or_frame_scan() {
    let code = r#"fn wrapper(depth) {
        if depth==0 {narrator("deep");} else {wrapper(depth-1);}
    } fn opening() {wrapper(16);}"#;
    for mode in [0, 2] {
        let (mut session, mut vm) = prepared(code, mode);
        let RunOutcome::HostCall(ticket) = next(&mut vm, &mut session) else {
            panic!("narrator")
        };
        let bound = vm.vm.host_call_sites(ticket).unwrap().frame_bound();
        assert!(bound > 16);
        let objects = vm.vm.heap().stats().live_objects;
        let NativeDispatch::BudgetYield {
            work: 0,
            required_work,
        } = vm
            .dispatch(&mut session, ticket, "en", NativeBudget::Exact(0))
            .unwrap()
        else {
            panic!("zero budget must retain ticket")
        };
        assert!(required_work >= bound as u32);
        assert!(session.view().interaction.is_none());
        assert!(session.view().history.is_empty());
        assert_eq!(vm.vm.heap().stats().live_objects, objects);
        assert!(vm.vm.host_call_sites(ticket).is_ok());
        assert!(matches!(
            vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(required_work-1)).unwrap(),
            NativeDispatch::BudgetYield {work:0,required_work:need} if need==required_work));
        let NativeDispatch::Published { work, .. } = vm
            .dispatch(
                &mut session,
                ticket,
                "en",
                NativeBudget::Exact(required_work),
            )
            .unwrap()
        else {
            panic!("exact minimum must publish")
        };
        assert!(work <= required_work && work >= bound as u32);
        assert_eq!(session.view().history.len(), 1);
        assert!(
            vm.dispatch(&mut session, ticket, "en", NativeBudget::RetryMinimum(0))
                .is_err()
        );
    }
}

fn restored(vm: &mut SessionVm, session: &Session) -> (Session, SessionVm) {
    use snapshot::{SessionSnapshot, SnapshotBudget};
    let program = vm.vm.program().clone();
    let mut capture = vm.snapshot_begin(session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut image = SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        program,
        VmLimits::default(),
        SessionIdentity {
            session_id: session.identity().session_id,
            epoch: Epoch(session.identity().epoch.0 + 1),
        },
        session.read_progress().clone(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    restore.finish().unwrap()
}

#[test]
fn native_callback_and_nested_wait_restore_preserves_paths_without_republishing() {
    let code = r#"
        fn inner(value) {narrator(value);}
        fn outer(value) {scope(body:||inner(value));}
        fn opening() {outer("first");outer("second");for i in 0..2 {outer("repeat");}}
    "#;
    let expected = play(code, 0, 4);
    for mode in [0, 2] {
        let (mut session, mut vm) = prepared(code, mode);
        for (index, expected) in expected.iter().enumerate() {
            models::drive(&mut vm, &mut session, true);
            assert_eq!(&observe(&session), expected);
            (session, vm) = restored(&mut vm, &session);
            models::drive(&mut vm, &mut session, true);
            assert_eq!(&observe(&session), expected);
            assert_eq!(session.view().history.len(), index + 1);
            advance(&mut session);
        }
        models::drive(&mut vm, &mut session, true);
        assert!(vm.task().is_none());
        assert_eq!(session.read_progress().ids().count(), 3);
    }
}

#[test]
fn grouped_identity_reserves_its_work_before_incremental_decoding_and_exact_retry() {
    let code = r#"fn group(who) {say([who.line("first"),who.line("second")]);}
        fn opening() {group(character("Speaker"));}"#;
    for mode in [0, 2] {
        let (mut session, mut vm) = prepared(code, mode);
        let ticket = loop {
            let RunOutcome::HostCall(ticket) = next(&mut vm, &mut session) else {
                panic!("native")
            };
            let p = vm.vm.program().package();
            if p.symbols[p.natives[ticket.native.index()].symbol.index()] == "reel::say" {
                break ticket;
            }
            vm.dispatch(&mut session, ticket, "en", NativeBudget::RetryMinimum(32))
                .unwrap();
        };
        let objects = vm.vm.heap().stats().live_objects;
        let NativeDispatch::BudgetYield {
            work: 0,
            required_work,
        } = vm
            .dispatch(&mut session, ticket, "en", NativeBudget::Exact(0))
            .unwrap()
        else {
            panic!("group admission")
        };
        assert!(vm.dialogue_reader.is_none());
        assert_eq!(vm.vm.heap().stats().live_objects, objects);
        assert!(session.view().history.is_empty());
        let mut calls = 0;
        let mut total_work = 0;
        loop {
            calls += 1;
            assert!(calls < 1000);
            match vm
                .dispatch(
                    &mut session,
                    ticket,
                    "en",
                    NativeBudget::Exact(required_work),
                )
                .unwrap()
            {
                NativeDispatch::Decoding { work } => {
                    assert!(work <= required_work);
                    total_work += work;
                    assert!(matches!(
                        vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(0)).unwrap(),
                        NativeDispatch::BudgetYield {work:0,required_work:need} if need==required_work));
                    assert!(session.view().history.is_empty());
                }
                NativeDispatch::Published { work, .. } => {
                    assert!(work <= required_work);
                    total_work += work;
                    break;
                }
                other => panic!("group progress: {other:?}"),
            }
        }
        assert!(calls > 1);
        assert!(total_work > required_work);
        assert_eq!(session.view().history.len(), 1);
        let observed = observe(&session);
        assert_eq!(observed.ids.len(), 2);
        assert_ne!(observed.ids[0], observed.ids[1]);
        advance(&mut session);
        models::drive(&mut vm, &mut session, true);
        assert_eq!(session.read_progress().ids().count(), 2);
        assert!(vm.task().is_none());
    }
}

#[test]
fn background_narrative_is_rejected_and_does_not_join_the_main_caller_path() {
    let code = r#"fn line() {narrator("main");} fn opening() {
        let worker=spawn(|| {
            let mut denied=false;
            try {narrator("background",wait:false);} catch error {denied=true;}
            if !denied {throw "background publication was admitted";}
        });
        await worker;
        for i in 0..2 {line();}
    }"#;
    let source = play(code, 0, 2);
    assert_eq!(source[0].text, "main");
    assert_eq!(source[0].ids, source[1].ids);
    assert!(!source[0].read);
    assert!(source[1].read);
    assert_eq!(play(code, 2, 2), source);
}
