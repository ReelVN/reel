//! Real ASB execution and snapshot round trips without a window or GPU.
use aside_vm::snapshot::{SnapshotBudget, SnapshotError};
use reel_cli::{BuildProfile, build_with_profile, check, decode_story};
use reel_engine::aside::{
    AdapterError, NativeDispatch, RunOutcome, SessionStep, SessionVm, SliceBudget, VmLimits,
};
use reel_engine::{Epoch, InputAction, InteractionId, Session, SessionId, SessionIdentity};

#[test]
fn release_keeps_runtime_data_and_faults_but_drops_resident_debug_data() {
    let source = format!(
        "// {}\nfn explode() {{ throw \"错误正文\"; }}\nfn main() {{ explode(); }}",
        "private author note ".repeat(4096),
    );
    let checked = check(&source, "story/opening.aside").unwrap();
    let debug = build_with_profile(&checked, Some("main::main"), BuildProfile::Debug).unwrap();
    let release = build_with_profile(&checked, Some("main::main"), BuildProfile::Release).unwrap();
    let d = decode_story(&debug.bytes).unwrap();
    let r = decode_story(&release.bytes).unwrap();
    assert_eq!(d.program.package().sources[0].text, source);
    assert!(r.program.package().sources.is_empty());
    assert!(
        r.program
            .package()
            .functions
            .iter()
            .all(|f| f.source_map.is_empty())
    );
    assert_eq!(d.program.package().constants, r.program.package().constants);
    assert_eq!(d.program.package().symbols, r.program.package().symbols);
    assert!(release.bytes.len() + source.len() < debug.bytes.len());
    assert!(
        r.program.stats().owned_capacity_bytes + source.len()
            < d.program.stats().owned_capacity_bytes
    );
    for (d, r) in d
        .program
        .package()
        .functions
        .iter()
        .zip(&r.program.package().functions)
    {
        assert_eq!(d.code, r.code);
        assert_eq!(d.descriptors, r.descriptors);
        assert_eq!(d.regions, r.regions);
    }
    for (artifact, location) in [
        (&debug, "story/opening.aside:2:16:"),
        (&release, "main::explode [function "),
    ] {
        let driver = reel_player::aside_session::AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            decode_story(&artifact.bytes).unwrap(),
            "zh-CN",
            VmLimits::default(),
        )
        .unwrap();
        let mut runtime = reel_player::story_runtime::StoryRuntime::new(driver);
        let error = (0..10_000)
            .find_map(|_| runtime.tick(SliceBudget::default(), || 0).err())
            .expect("script must throw")
            .to_string();
        assert!(error.contains(location), "{error}");
        assert!(error.contains("Thrown: 错误正文"), "{error}");
        if artifact.profile == BuildProfile::Release {
            assert!(error.contains(", pc "), "{error}");
        }
    }
    let repeated = build_with_profile(&checked, Some("main::main"), BuildProfile::Release).unwrap();
    assert_eq!(repeated.bytes, release.bytes);
}

fn until_dialogue(vm: &mut SessionVm, session: &mut Session) -> InteractionId {
    for _ in 0..10_000 {
        match vm
            .step(
                session,
                reel_engine::aside::SessionBudget::Exact(SliceBudget {
                    instructions: 7,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                if let NativeDispatch::Published { interaction, .. } = vm
                    .dispatch(
                        session,
                        ticket,
                        "zh-CN",
                        reel_engine::aside::NativeBudget::Exact(64),
                    )
                    .unwrap()
                {
                    return interaction;
                }
            }
            SessionStep::Vm(RunOutcome::BudgetYield(_)) => {}
            other => panic!("expected dialogue: {other:?}"),
        }
    }
    panic!("story did not reach dialogue");
}

#[test]
fn both_profiles_restore_the_same_artifact_without_replaying_initialization() {
    let source = r#"
        let mut entries = 0;
        class Counter {
            let mut value: int = 0;
            fn next(self) { self.value += 1; self.value }
        }
        fn main() {
            entries += 1;
            let counter = Counter();
            let offset = 7;
            let add = |value| value + offset;
            narrator("保存位置");
            narrator(f"恢复 {add(counter.next())}，入口 {entries}");
        }
    "#;
    let checked = check(source, "main.aside").unwrap();
    let artifacts = [BuildProfile::Debug, BuildProfile::Release]
        .map(|profile| build_with_profile(&checked, Some("main::main"), profile).unwrap());
    for (index, artifact) in artifacts.iter().enumerate() {
        let story = decode_story(&artifact.bytes).unwrap();
        let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
        let mut vm = SessionVm::load(&session, story.program.clone(), VmLimits::default()).unwrap();
        vm.start(&session, story.initializer).unwrap();
        loop {
            match vm
                .step(
                    &mut session,
                    reel_engine::aside::SessionBudget::Exact(SliceBudget::default()),
                    || 0,
                )
                .unwrap()
            {
                SessionStep::Vm(RunOutcome::BudgetYield(_)) => {}
                SessionStep::Vm(RunOutcome::Returned(_)) => {
                    vm.discard_delivery(&mut session).unwrap();
                    break;
                }
                other => panic!("initializer: {other:?}"),
            }
        }
        vm.start(&session, story.entry).unwrap();
        let original_interaction = until_dialogue(&mut vm, &mut session);
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 16 }).unwrap().complete {}
        let bytes = capture.finish().unwrap().encode().unwrap();
        let identity = SessionIdentity {
            session_id: SessionId(2),
            epoch: Epoch(2),
        };
        let mut wrong_image =
            reel_engine::aside::snapshot::SessionSnapshot::decode(&bytes).unwrap();
        let wrong_program = decode_story(&artifacts[1 - index].bytes).unwrap().program;
        let wrong = SessionVm::restore_begin(
            &mut wrong_image,
            wrong_program,
            VmLimits::default(),
            identity,
            Default::default(),
            None,
            None,
        );
        assert!(matches!(
            wrong,
            Err(AdapterError::Snapshot(SnapshotError::CodeMismatch))
        ));

        let mut image = reel_engine::aside::snapshot::SessionSnapshot::decode(&bytes).unwrap();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            story.program,
            VmLimits::default(),
            identity,
            Default::default(),
            None,
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 16 }).unwrap().complete {}
        let (mut session, mut vm) = restore.finish().unwrap();
        let owner = session.root_owner();
        let interaction = session.view().interaction.unwrap().id();
        assert_ne!(interaction, original_interaction);
        session
            .handle_local_input(owner, InputAction::Advance(interaction))
            .unwrap();
        session
            .handle_local_input(owner, InputAction::Advance(interaction))
            .unwrap();
        until_dialogue(&mut vm, &mut session);
        assert_eq!(
            session.view().history.last().unwrap().lines()[0].resolved_text(),
            "恢复 8，入口 1"
        );
    }
}
