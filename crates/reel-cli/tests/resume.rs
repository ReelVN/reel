//! Stable continuations survive the ordinary source, release and ASB paths.
use reel_cli::BuildProfile;
use reel_engine::aside::{
    NativeBudget, RunOutcome, SessionBudget, SessionStep, SessionVm, SliceBudget, VmLimits,
};
use reel_engine::{Epoch, Session, SessionId};
use reel_package as rp;

const SOURCE: &str = r#"
    use std::resume::{state, root, flow, point};
    class Profile { let mut score:int=41; }
    state(Profile,"profile"); root(main,"game");
    fn child(score:int) {
        flow("chapter.child");
        point("dialogue");
        narrator("Current release text.");
        score + 1
    }
    fn opening(state:Profile) {
        flow("chapter.main");
        let bonus:int = 7;
        point("child-result");
        let result = child(state.score);
        bonus + result
    }
    fn main(state:Profile=model(Profile())){
        run_flow(opening,state)
    }
"#;

fn advance(vm: &mut SessionVm, session: &mut Session) -> RunOutcome {
    for _ in 0..20_000 {
        match vm
            .step(
                session,
                SessionBudget::Exact(SliceBudget {
                    instructions: 1,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::BudgetYield(_)) | SessionStep::Progress => {}
            SessionStep::Vm(outcome) => return outcome,
            outcome => panic!("unexpected session step: {outcome:?}"),
        }
    }
    panic!("continuation fixture exhausted its work budget")
}

#[test]
fn stable_stack_is_available_after_debug_release_and_external_text_reencoding() {
    let checked = reel_cli::check(SOURCE, "resume.aside").unwrap();
    for profile in [BuildProfile::Debug, BuildProfile::Release] {
        let artifact = reel_cli::build_export(&checked, "main::main", profile).unwrap();
        let text_pool = artifact.text_pool.clone();
        let mut package = rp::decode(
            &artifact.bytes,
            reel_engine::aside::source::types(),
            Default::default(),
        )
        .unwrap();
        assert_eq!(package.resume().table(), &checked.compilation.resume);
        assert_eq!(package.resume().table().flows.len(), 2);
        if profile == BuildProfile::Release {
            assert!(package.program().package().sources.is_empty());
            package = package.bind_text_pool(text_pool.clone().unwrap()).unwrap();
        }
        let encoded = package.program().encode(Default::default()).unwrap();
        let rebound = rp::encode(
            &encoded,
            package.metadata(),
            package.distribution(),
            package.effects(),
            package.resume().table(),
            reel_engine::aside::source::types(),
            Default::default(),
        )
        .unwrap();
        let mut decoded = rp::decode(
            &rebound,
            reel_engine::aside::source::types(),
            Default::default(),
        )
        .unwrap();
        if let Some(pool) = text_pool {
            decoded = decoded.bind_text_pool(pool).unwrap();
        }
        let story = reel_player::distribution::story_from_package(decoded).unwrap();
        let (initializer, entry) = (story.initializer, story.entry);
        let mut session = Session::new(SessionId(301), Epoch(1)).unwrap();
        let mut vm = SessionVm::load_story(&session, story, VmLimits::default()).unwrap();
        vm.start(&session, initializer).unwrap();
        assert!(matches!(
            advance(&mut vm, &mut session),
            RunOutcome::Returned(_)
        ));
        vm.discard_delivery(&mut session).unwrap();
        let task = vm.start(&session, entry).unwrap();
        loop {
            match advance(&mut vm, &mut session) {
                RunOutcome::HostCall(ticket) => {
                    vm.dispatch(&mut session, ticket, "en", NativeBudget::RetryMinimum(64))
                        .unwrap();
                }
                RunOutcome::Waiting(_) => break,
                outcome => panic!("expected dialogue wait: {outcome:?}"),
            }
        }
        let (flow_task, boundary) = vm.flow_boundary().unwrap();
        assert_eq!(flow_task, task);
        let mut capture = vm
            .vm()
            .capture_resume(vm.resume_table(), task, boundary)
            .unwrap();
        assert_eq!(capture.descriptor().frames.len(), 2);
        let mut values = Vec::new();
        while let Some(binding) = capture.next_binding().unwrap() {
            values.push((binding.frame, binding.key.0.clone(), binding.value.as_int()));
        }
        assert!(values.contains(&(0, "bonus".into(), Some(7))));
        assert!(values.contains(&(1, "score".into(), Some(41))));
    }
}

#[test]
fn asb_writer_rejects_a_resume_pc_from_another_layout() {
    let checked = reel_cli::check(SOURCE, "resume.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    let package = rp::decode(
        &artifact.bytes,
        reel_engine::aside::source::types(),
        Default::default(),
    )
    .unwrap();
    let mut invalid = package.resume().table().clone();
    invalid
        .flows
        .iter_mut()
        .find_map(|flow| flow.points.first_mut())
        .unwrap()
        .call
        .0 = u32::MAX;
    let error = rp::encode(
        &package.program().encode(Default::default()).unwrap(),
        package.metadata(),
        None,
        package.effects(),
        &invalid,
        reel_engine::aside::source::types(),
        Default::default(),
    )
    .unwrap_err();
    assert_eq!(error.kind, rp::ErrorKind::Metadata);
}

#[test]
fn state_declarations_survive_asb_without_story_flows() {
    let checked = reel_cli::check(
        r#"
            use std::resume::{state, root};
            class Profile { let score:int=0; }
            state(Profile,"profile");
            root(main,"game");
            fn main(value:Profile=Profile()){value}
        "#,
        "state-only.aside",
    )
    .unwrap();
    let table = &checked.compilation.resume;
    assert!(table.flows.is_empty());
    assert_eq!(table.states.len(), 1);
    assert!(table.root.is_some());
    for profile in [BuildProfile::Debug, BuildProfile::Release] {
        let artifact = reel_cli::build_export(&checked, "main::main", profile).unwrap();
        let mut package = rp::decode(
            &artifact.bytes,
            reel_engine::aside::source::types(),
            Default::default(),
        )
        .unwrap();
        if let Some(pool) = artifact.text_pool {
            package = package.bind_text_pool(pool).unwrap();
        }
        assert_eq!(package.resume().table(), table);
        assert_eq!(package.resume().table().version, 1);
        let encoded = rp::encode(
            &package.program().encode(Default::default()).unwrap(),
            package.metadata(),
            package.distribution(),
            package.effects(),
            package.resume().table(),
            reel_engine::aside::source::types(),
            Default::default(),
        )
        .unwrap();
        let decoded = rp::decode(
            &encoded,
            reel_engine::aside::source::types(),
            Default::default(),
        )
        .unwrap();
        assert_eq!(decoded.resume().table(), table);
    }
}
