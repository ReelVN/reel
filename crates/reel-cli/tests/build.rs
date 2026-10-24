//! Author CLI build processes only compile and validate; no Aside runs.
use reel_package as rp;
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

struct Case {
    dir: PathBuf,
}
impl Case {
    fn new(source: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/reel-artifacts/build-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.aside"), source).unwrap();
        Self { dir }
    }
    fn run(&self, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_reel"))
            .current_dir(&self.dir)
            .args(["build", "--output", "result.asb", "--format", "json"])
            .args(extra)
            .output()
            .unwrap()
    }
    fn bytes(&self) -> Vec<u8> {
        fs::read(self.dir.join("result.asb")).unwrap()
    }
    fn assert_no_staging(&self) {
        assert!(fs::read_dir(&self.dir).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".reel-stage-")
        }));
    }
}
impl Drop for Case {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.dir).unwrap();
    }
}

#[test]
fn builds_fixed_project_entry_and_library_private_entry_reproducibly() {
    let case = Case::new("fn main() { 1 } fn hidden(a = 1) { loop {} }");
    let output = case.run(&[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "built");
    assert_eq!(report["artifact"]["module_only"], false);
    assert_eq!(report["artifact"]["profile"], "debug");
    assert_eq!(report["artifact"]["source_included"], true);
    assert!(report["artifact"]["game_entry"].is_object());
    assert_eq!(report["executed"], false);
    assert_eq!(report["artifact"]["power_loss_durability"], "not_promised");
    assert_eq!(report["artifact"]["blake3"], rp::digest(&case.bytes()));
    let story = reel_cli::decode_story(&case.bytes()).unwrap();
    let package = story.program.package();
    assert_eq!(
        package.symbols[package.functions[story.entry.index()].name.index()],
        "main"
    );
    assert_eq!(
        package.symbols[package.sources[0].path.index()],
        "src/main.aside"
    );
    let checked =
        reel_cli::check("fn main() { 1 } fn hidden(a = 1) { loop {} }", "unit.aside").unwrap();
    let private = reel_cli::build(&checked, Some("main::hidden")).unwrap();
    let story = reel_cli::decode_story(&private.bytes).unwrap();
    let package = story.program.package();
    assert_eq!(
        package.symbols[package.functions[story.entry.index()].name.index()],
        "hidden"
    );
    assert_eq!(
        reel_cli::build(&checked, Some("main::hidden"))
            .unwrap()
            .bytes,
        private.bytes
    );
    let before = case.bytes();
    assert!(case.run(&["--overwrite"]).status.success());
    assert_eq!(case.bytes(), before);
    case.assert_no_staging();
}

#[test]
fn release_build_strips_debug_data_and_check_dev_reject_release() {
    let case = Case::new("// private author note\nfn main() { \"正文不能删除\" }");
    let output = case.run(&["--release"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["artifact"]["profile"], "release");
    assert_eq!(report["artifact"]["source_included"], false);
    let story = reel_cli::decode_story(&case.bytes()).unwrap();
    assert!(story.program.package().sources.is_empty());
    assert!(
        story
            .program
            .package()
            .functions
            .iter()
            .all(|f| f.source_map.is_empty())
    );
    assert_eq!(report["encoded_bytes"], story.program.stats().encoded_bytes);
    assert!(
        !case
            .bytes()
            .windows(b"private author note".len())
            .any(|bytes| bytes == b"private author note")
    );
    for command in ["check", "dev"] {
        let output = Command::new(env!("CARGO_BIN_EXE_reel"))
            .args([command, "--release", "--format", "json"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["stage"], "usage");
        assert!(
            report["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .contains("--release")
        );
    }
}

#[test]
fn function_value_example_builds_and_runs_with_stage_coordinates_and_real_wait() {
    let text = author_example(
        include_str!("../../../examples/function_values/src/main.aside"),
        &[(32., 48.), (32., 90.), (32., 150.)],
    );
    assert_eq!(
        text,
        [
            "函数可以保存、传递，也可以作为缺省参数。",
            "坐标使用 (int,int)，省略时采用缺省位置。",
            "通过函数变量调用旁白，照常等待点击后继续。",
        ]
    );
    author_example(
        include_str!("../../../examples/collections/src/main.aside"),
        &[(32., 48.), (32., 90.), (32., 132.)],
    );
    author_example(
        include_str!("../../../examples/collection_editing/src/main.aside"),
        &[(0., 0.); 7],
    );
    let text = author_example(
        include_str!("../../../examples/text_operations/src/main.aside"),
        &[(0., 0.); 8],
    );
    assert_eq!(
        text,
        [
            "序章 · 窗上的光 · 下一幕",
            "序章",
            "窗上的光",
            "下一幕",
            "首项：序章",
            "中🙂é：10 字节，4 个标量",
            "玩家名：空 山",
            "章节顺序正确",
        ]
    );
}

fn author_example(source: &str, coordinates: &[(f64, f64)]) -> Vec<String> {
    use reel_engine::aside::{
        GcBudget, NativeDispatch, RunOutcome, SessionVm, SliceBudget, VmLimits,
    };
    use reel_engine::{Epoch, InputAction, Session, SessionId};

    let case = Case::new(source);
    let output = case.run(&[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let story = reel_cli::decode_story(&case.bytes()).unwrap();
    let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
    let mut vm = SessionVm::load(&session, story.program, VmLimits::default()).unwrap();
    let mut publications = 0;
    let mut published = None;
    let mut text = Vec::new();
    for entry in [story.initializer, story.entry] {
        vm.start(&session, entry).unwrap();
        let mut returned = false;
        for _ in 0..10_000 {
            let outcome = vm
                .step(
                    &mut session,
                    reel_engine::aside::SessionBudget::Exact(SliceBudget {
                        instructions: 1,
                        ..Default::default()
                    }),
                    || 0,
                )
                .unwrap();
            let outcome = match outcome {
                reel_engine::aside::SessionStep::Vm(outcome) => outcome,
                reel_engine::aside::SessionStep::Progress => continue,
                other => panic!("expected main task progress: {other:?}"),
            };
            match outcome {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::HostCall(ticket) => {
                    if let NativeDispatch::Published { interaction, .. } = vm
                        .dispatch(
                            &mut session,
                            ticket,
                            "zh-CN",
                            reel_engine::aside::NativeBudget::Exact(256),
                        )
                        .unwrap()
                    {
                        publications += 1;
                        published = Some(interaction);
                    }
                }
                RunOutcome::Waiting(_) => {
                    if !vm.stage_ready() {
                        continue;
                    }
                    let interaction = published.take().expect("published dialogue wait");
                    vm.sync_stage(&session).unwrap();
                    let stage = vm.stage();
                    let children = &stage.node(stage.root().unwrap()).unwrap().children;
                    assert_eq!(children.len(), coordinates.len());
                    for (id, expected) in children.iter().zip(coordinates) {
                        let node = stage.node(*id).unwrap();
                        let props = &node.props;
                        assert_eq!((props.x, props.y), *expected);
                        if let reel_ui::stage::NodeKind::Text(value) = &node.kind {
                            text.push(value.plain().to_string());
                        }
                    }
                    vm.begin_collection().unwrap();
                    while vm.vm().heap().phase() != aside_vm::GcPhase::Idle {
                        vm.gc_step(GcBudget {
                            units: 1,
                            ..Default::default()
                        })
                        .unwrap();
                    }
                    let owner = session.root_owner();
                    session
                        .handle_local_input(owner, InputAction::Advance(interaction))
                        .unwrap();
                    session
                        .handle_local_input(owner, InputAction::Advance(interaction))
                        .unwrap();
                }
                RunOutcome::Returned(_) => {
                    vm.discard_delivery(&mut session).unwrap();
                    returned = true;
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(returned);
    }
    assert_eq!(publications, 1);
    assert_eq!(session.view().history.len(), 1);
    assert!(vm.vm().is_idle());
    text
}

#[test]
fn explicit_output_survives_no_overwrite_and_all_source_failures() {
    let case = Case::new("fn main() { 1 }");
    fs::write(case.dir.join("result.asb"), b"original bytes").unwrap();
    let output = case.run(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(case.bytes(), b"original bytes");
    for source in [
        "let =;",
        "missing();",
        "use reel::story;",
        "fn main() { let [a]=[1]; a }",
    ] {
        fs::write(case.dir.join("src/main.aside"), source).unwrap();
        let output = case.run(&["--overwrite"]);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(case.bytes(), b"original bytes");
        case.assert_no_staging();
    }
}

#[test]
fn bad_entry_has_source_location_and_never_publishes() {
    let case = Case::new("fn main(required) { required }");
    let output = case.run(&[]);
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["stage"], "entry");
    assert!(report["diagnostics"][0]["range"]["start"]["line"].is_number());
    assert!(!case.dir.join("result.asb").exists());
    case.assert_no_staging();
}

#[test]
fn explicit_output_rules_and_source_protection() {
    let case = Case::new("fn main() { 1 }");
    for args in [
        vec!["build", "other.aside", "--format", "json"],
        vec!["build", "--output", "bad.rpk", "--format", "json"],
        vec!["build", "--output", "missing/out.asb", "--format", "json"],
        vec!["check", "--entry", "main::main", "--format", "json"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_reel"))
            .current_dir(&case.dir)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
    let source = "fn main() { 1 }";
    fs::write(case.dir.join("self.asb"), source).unwrap();
    let file = case.dir.join("self.asb");
    assert!(reel_cli::output::write(&file, &file, b"replacement", true).is_err());
    assert_eq!(
        fs::read_to_string(case.dir.join("self.asb")).unwrap(),
        source
    );
    case.assert_no_staging();
}

#[cfg(unix)]
#[test]
fn output_symlinks_and_directories_are_not_replaced() {
    let case = Case::new("fn main() { 1 }");
    fs::write(case.dir.join("victim"), b"keep").unwrap();
    std::os::unix::fs::symlink("victim", case.dir.join("result.asb")).unwrap();
    for options in [vec![], vec!["--overwrite"]] {
        assert_eq!(case.run(&options).status.code(), Some(2));
        assert_eq!(fs::read(case.dir.join("victim")).unwrap(), b"keep");
    }
    fs::remove_file(case.dir.join("result.asb")).unwrap();
    fs::create_dir(case.dir.join("result.asb")).unwrap();
    assert_eq!(case.run(&["--overwrite"]).status.code(), Some(2));
    assert!(case.dir.join("result.asb").is_dir());
    case.assert_no_staging();
}

#[test]
fn concurrent_explicit_outputs_publish_exactly_one_complete_artifact() {
    let case = Case::new("fn main() { 1 }");
    let results = std::thread::scope(|scope| {
        let a = scope.spawn(|| case.run(&[]));
        let b = scope.spawn(|| case.run(&[]));
        [a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.status.success()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| r.status.code() == Some(2))
            .count(),
        1
    );
    rp::decode(&case.bytes(), rp::Limits::default()).unwrap();
    case.assert_no_staging();
}

mod build_profiles {
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
        let release =
            build_with_profile(&checked, Some("main::main"), BuildProfile::Release).unwrap();
        let d = decode_story(&debug.bytes).unwrap();
        let r = decode_story(&release.bytes).unwrap();
        assert_eq!(d.program.package(), checked.compilation.program.package());
        let encoded = reel_package::decode(&debug.bytes, Default::default()).unwrap();
        assert_eq!(encoded.metadata().modules, checked.modules);
        assert_eq!(encoded.metadata().game_entry, debug.game_entry);
        let mut expected = checked.compilation.program.package().clone();
        expected.sources.clear();
        for function in &mut expected.functions {
            function.source_map = aside_vm::bytecode::CompactSourceMap::default();
        }
        assert_eq!(r.program.package(), &expected);
        let encoded = reel_package::decode(&release.bytes, Default::default()).unwrap();
        let mut expected_modules = checked.modules.clone();
        for module in &mut expected_modules {
            module.debug = None;
        }
        assert_eq!(encoded.metadata().modules, expected_modules);
        assert_eq!(encoded.metadata().game_entry, release.game_entry);
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
        assert!(release.bytes.len() < debug.bytes.len());
        assert!(debug.bytes.len() - release.bytes.len() < source.len());
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
        let repeated =
            build_with_profile(&checked, Some("main::main"), BuildProfile::Release).unwrap();
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
            let mut vm =
                SessionVm::load(&session, story.program.clone(), VmLimits::default()).unwrap();
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
}
