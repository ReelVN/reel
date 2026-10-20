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
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("input.aside"), source).unwrap();
        Self { dir }
    }
    fn run(&self, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_reel"))
            .current_dir(&self.dir)
            .args([
                "build",
                "input.aside",
                "--output",
                "result.asb",
                "--format",
                "json",
            ])
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
fn builds_default_and_explicit_private_entry_reproducibly() {
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
        "input.aside"
    );
    let output = case.run(&["--entry", "main::hidden", "--overwrite"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let bytes = case.bytes();
    let read = rp::decode(&bytes, rp::Limits::default()).unwrap();
    assert!(read.metadata().game_entry.is_some());
    let output = case.run(&["--entry", "main::hidden", "--overwrite"]);
    assert!(output.status.success());
    assert_eq!(case.bytes(), bytes);
    case.assert_no_staging();
}

#[test]
fn release_flag_strips_debug_data_and_is_build_only() {
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
    for command in ["check", "dev", "run"] {
        let output = Command::new(env!("CARGO_BIN_EXE_reel"))
            .args([command, "input.aside", "--release", "--format", "json"])
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
    author_example(
        include_str!("../../../examples/function_values.aside"),
        &[(32., 48.), (32., 90.)],
    );
    author_example(
        include_str!("../../../examples/collections.aside"),
        &[(32., 48.), (32., 90.), (32., 132.)],
    );
    author_example(
        include_str!("../../../examples/collection_editing.aside"),
        &[(0., 0.); 7],
    );
    let text = author_example(
        include_str!("../../../examples/text_operations.aside"),
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
            let reel_engine::aside::SessionStep::Vm(outcome) = outcome else {
                panic!("expected main task progress");
            };
            match outcome {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::HostCall(ticket) => {
                    if let NativeDispatch::Published { interaction, .. } =
                        vm.dispatch(&mut session, ticket, "zh-CN", 256).unwrap()
                    {
                        publications += 1;
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
fn existing_output_survives_default_publication_and_all_source_failures() {
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
        fs::write(case.dir.join("input.aside"), source).unwrap();
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
        vec!["build", "input.aside", "--format", "json"],
        vec![
            "build",
            "input.aside",
            "--output",
            "bad.rpk",
            "--format",
            "json",
        ],
        vec![
            "build",
            "input.aside",
            "--output",
            "missing/out.asb",
            "--format",
            "json",
        ],
        vec![
            "check",
            "input.aside",
            "--entry",
            "main::main",
            "--format",
            "json",
        ],
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
    let output = Command::new(env!("CARGO_BIN_EXE_reel"))
        .current_dir(&case.dir)
        .args([
            "build",
            "self.asb",
            "--output",
            "self.asb",
            "--overwrite",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
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
fn concurrent_default_builds_publish_exactly_one_complete_artifact() {
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
