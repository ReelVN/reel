use super::*;
use aside_vm::resume::{
    ContinuationDescriptor, FlowKey, FrameDescriptor, PointKey, ResumeLimits, ResumeSite, ReturnKey,
};

const A: &str = r#"
use std::resume::{state,root,flow,point};
class World {let mut score:int=0;}
state(World,"world");root(start,"game");
fn chapter(value:World){flow("chapter");point("line");narrator("Original text");value.score}
fn story(value:World){
    flow("story");
    point("first");chapter(value);
    point("second");chapter(value);
}
fn start(value:World=model(World())){run_flow(story,value);}
fn opening(){start();}
"#;
const B: &str = r#"
use std::resume::{state,root,flow,point};
fn unrelated(){flow("unrelated");17}
class CurrentWorld {let extra:int=99;let mut score:int=0;}
state(CurrentWorld,"world");root(current_start,"game");
fn current_chapter(value:CurrentWorld){
    flow("chapter");value.score+=1;
    point("line");narrator("Current translated text");value.score
}
fn current_story(value:CurrentWorld){
    flow("story");value.score+=1;
    point("first");current_chapter(value);
    point("second");current_chapter(value);
}
fn current_start(value:CurrentWorld=model(CurrentWorld())){run_flow(current_story,value);}
fn opening(){current_start();}
"#;

fn prepared_logical(code: &str, version: u64, mode: usize) -> (Session, SessionVm) {
    let mut story = source::compile_story(code, "logical_identity", version, "opening").unwrap();
    if mode != 0 {
        let mut package = story.program.package().clone();
        if mode == 2 {
            for function in &mut package.functions {
                function.source_map = bc::CompactSourceMap::default();
            }
            package.sources.clear();
        }
        story.program = bc::VerifiedProgram::decode(
            &bc::encode(&package, Default::default()).unwrap(),
            Default::default(),
        )
        .unwrap();
        story.resume = story
            .program
            .verify_resume(
                story.resume.table().clone(),
                crate::aside::source::types(),
                Default::default(),
            )
            .unwrap();
    }
    let (initializer, entry) = (story.initializer, story.entry);
    let mut session = session();
    let mut vm = SessionVm::load_story(&session, story, VmLimits::default()).unwrap();
    vm.start(&session, initializer).unwrap();
    models::drive(&mut vm, &mut session, false);
    vm.start(&session, entry).unwrap();
    (session, vm)
}
fn line_ticket(vm: &mut SessionVm, session: &mut Session) -> HostTicket {
    loop {
        let RunOutcome::HostCall(ticket) = next(vm, session) else {
            panic!("expected a dialogue call")
        };
        let p = vm.vm.program().package();
        if matches!(
            p.symbols[p.natives[ticket.native.index()].symbol.index()].as_str(),
            "reel::narrator" | "reel::character::speak"
        ) {
            return ticket;
        }
        vm.dispatch(session, ticket, "en", NativeBudget::RetryMinimum(1))
            .unwrap();
    }
}

#[test]
fn nominal_character_calls_keep_mutable_receiver_identity_and_registered_target_source_asb() {
    let code = r#"
        use std::resume::{state,root,flow,point};
        class World{let marker:int=1;}state(World,"world");root(start,"game");
        fn story(value:World){
            flow("characters");
            let mut voice=character("Unknown",id:"speaker");
            point("unknown");voice("First line");
            voice=character("Named",id:"speaker");
            point("named");voice("Second line");
            value.marker
        }
        fn start(value:World=model(World())){run_flow(story,value);}
        fn opening(){start();}
    "#;
    for mode in [0, 2] {
        let (mut session, mut vm) = prepared_logical(code, 31, mode);
        assert!(
            vm.vm
                .program()
                .verify_resume(
                    vm.resume_table().table().clone(),
                    &[],
                    ResumeLimits::default()
                )
                .is_err()
        );
        let mut ids = Vec::new();
        for (name, text) in [("Unknown", "First line"), ("Named", "Second line")] {
            let ticket = line_ticket(&mut vm, &mut session);
            let table = vm.resume_table().clone();
            let proof = vm
                .vm
                .resume_descriptor(
                    &table,
                    ticket,
                    vm.flow_boundary().unwrap().1,
                    ResumeLimits::default(),
                )
                .unwrap();
            assert_eq!(proof.frame_count(), 1);
            vm.dispatch(&mut session, ticket, "en", NativeBudget::RetryMinimum(1))
                .unwrap();
            let Some(InteractionView::Dialogue(line)) = session.view().interaction else {
                panic!("character dialogue")
            };
            assert_eq!(line.speaker_name(), name);
            assert_eq!(line.text(), text);
            ids.push(line.lines()[0].content_id().to_owned());
            advance(&mut session);
        }
        assert_ne!(ids[0], ids[1]);
        models::drive(&mut vm, &mut session, false);
        assert!(vm.task().is_none());
    }
}
fn logical_play(code: &str, version: u64, mode: usize) -> Vec<Observation> {
    let (mut session, mut vm) = prepared_logical(code, version, mode);
    let mut observations = Vec::new();
    for _ in 0..2 {
        let ticket = line_ticket(&mut vm, &mut session);
        assert!(matches!(
            vm.dispatch(&mut session, ticket, "en", NativeBudget::RetryMinimum(1))
                .unwrap(),
            NativeDispatch::Published { .. }
        ));
        observations.push(observe(&session));
        advance(&mut session);
    }
    models::drive(&mut vm, &mut session, false);
    assert!(vm.task().is_none());
    observations
}
fn expected_descriptor() -> ContinuationDescriptor {
    ContinuationDescriptor {
        frames: vec![
            FrameDescriptor {
                flow: FlowKey("story".into()),
                site: ResumeSite::Return(ReturnKey("first".into())),
            },
            FrameDescriptor {
                flow: FlowKey("chapter".into()),
                site: ResumeSite::Point(PointKey("line".into())),
            },
        ],
    }
}

#[test]
fn unmarked_dialogue_and_parent_calls_receive_logical_identities() {
    let partial = logical_play(&A.replace("point(\"first\");", ""), 1, 2);
    assert!(partial[0].ids[0].starts_with("flow:v1:"));
    assert!(partial[1].ids[0].starts_with("flow:v1:"));

    let unmarked = logical_play(&A.replace("point(\"line\");", ""), 1, 2);
    assert!(
        unmarked
            .iter()
            .all(|line| line.ids[0].starts_with("flow:v1:"))
    );
}

#[test]
fn stable_identity_uses_root_and_logical_return_path_across_current_code_layouts() {
    let old = logical_play(A, 1, 0);
    assert_ne!(old[0].ids, old[1].ids);
    assert!(old[0].ids[0].starts_with("flow:v1:"));
    for mode in [0, 1, 2] {
        let current = logical_play(B, 27, mode);
        assert_eq!(
            old.iter().map(|o| &o.ids).collect::<Vec<_>>(),
            current.iter().map(|o| &o.ids).collect::<Vec<_>>()
        );
        assert_ne!(old[0].text, current[0].text);
        let root = logical_play(&B.replace("\"game\"", "\"other-game\""), 27, mode);
        assert_ne!(current[0].ids, root[0].ids);
        let point = logical_play(
            &B.replace("point(\"line\")", "point(\"other-line\")"),
            27,
            mode,
        );
        assert_ne!(current[0].ids, point[0].ids);
        let path = logical_play(
            &B.replace("point(\"second\")", "point(\"other-return\")"),
            27,
            mode,
        );
        assert_eq!(current[0].ids, path[0].ids);
        assert_ne!(current[1].ids, path[1].ids);
    }
}

#[test]
fn issued_and_waiting_share_the_stack_proof_and_borrow_stable_keys() {
    for mode in [0, 2] {
        let (mut session, mut vm) = prepared_logical(A, 1, mode);
        let ticket = line_ticket(&mut vm, &mut session);
        let (task, boundary) = vm.flow_boundary().unwrap();
        let table = vm.resume_table().clone();
        let bound = vm.vm.resume_descriptor_bound(&table, ticket).unwrap();
        let expected = expected_descriptor();
        let charged = vm.vm.heap().stats().charged_bytes;
        let proof = vm
            .vm
            .resume_descriptor(
                &table,
                ticket,
                boundary,
                ResumeLimits {
                    work: bound.work,
                    allocation_bytes: 0,
                },
            )
            .unwrap();
        assert_eq!(proof.frame_count(), 2);
        assert!(proof.matches(&expected));
        assert_eq!(proof.frames().len(), 2);
        assert_eq!(
            proof
                .frames()
                .map(|f| f.flow.0.len() as u64 + f.site.key().len() as u64)
                .sum::<u64>(),
            proof.key_bytes()
        );
        assert_eq!(vm.vm.heap().stats().charged_bytes, charged);
        assert!(
            vm.vm
                .resume_descriptor(
                    &table,
                    ticket,
                    boundary,
                    ResumeLimits {
                        work: bound.work - 1,
                        allocation_bytes: 0
                    }
                )
                .is_err()
        );
        let mut forged = ticket;
        forged.frame = aside_vm::executor::FrameId(u64::MAX);
        assert!(vm.vm.resume_descriptor_bound(&table, forged).is_err());
        let NativeDispatch::BudgetYield {
            work: 0,
            required_work,
        } = vm
            .dispatch(&mut session, ticket, "en", NativeBudget::Exact(0))
            .unwrap()
        else {
            panic!("admission")
        };
        assert!(required_work as u64 >= bound.work);
        assert!(session.view().interaction.is_none());
        assert_eq!(vm.issued, Some(ticket));
        assert!(
            matches!(vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(required_work - 1)).unwrap(), NativeDispatch::BudgetYield {work: 0, required_work: n} if n == required_work)
        );
        assert!(
            matches!(vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(required_work)).unwrap(), NativeDispatch::Published {work, ..} if work <= required_work)
        );
        assert!(vm.vm.resume_descriptor_bound(&table, ticket).is_err());
        let capture = vm.vm.capture_resume(&table, task, boundary).unwrap();
        assert_eq!(capture.descriptor(), &expected);
    }
}

#[test]
fn checkpoint_rejects_an_undeclared_interaction_at_its_current_source_position() {
    let code = A.replace(
        "point(\"line\");narrator(\"Original text\");",
        "let ignored = [narrator(\"Original text\")];",
    );
    let (mut session, mut vm) = prepared_logical(&code, 1, 0);
    let ticket = line_ticket(&mut vm, &mut session);
    vm.dispatch(&mut session, ticket, "en", NativeBudget::RetryMinimum(1))
        .unwrap();
    assert!(matches!(
        next(&mut vm, &mut session),
        RunOutcome::Waiting(_)
    ));
    let (task, boundary) = vm.flow_boundary().unwrap();
    let Err(error) = vm.vm.capture_resume(vm.resume_table(), task, boundary) else {
        panic!("undeclared wait must fail checkpoint capture");
    };
    assert_eq!(error.function, Some(ticket.location.code.function));
    assert_eq!(error.pc, Some(ticket.location.pc));
    assert!(error.detail.contains("no declared point"));
    assert_eq!(session.view().history.len(), 1);
    assert!(session.view().interaction.is_some());
}
