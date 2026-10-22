//! Creator translations remain in source and enter the ordinary compile/runtime path.
use aside_vm::executor::RunOutcome;
use reel_cli::{into_story, project};
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{SessionBudget, SessionStep, SessionVm, SliceBudget, VmLimits},
};
use reel_ui::stage::NodeKind;
use std::{fs, path::Path};
fn write(root: &Path, path: &str, source: &str) {
    let path = root.join("src").join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}
fn run(checked: reel_cli::Checked) -> SessionVm {
    let story = into_story(checked, project::ENTRY).unwrap();
    let entry = story.entry;
    let initializer = story.initializer;
    let mut session = Session::new(SessionId(901), Epoch(1)).unwrap();
    let mut vm = SessionVm::load_story(&session, story, VmLimits::default()).unwrap();
    for function in [initializer, entry] {
        vm.start(&session, function).unwrap();
        for step in 0..100_000 {
            assert!(step < 99_999, "source catalog runtime stalled");
            vm.maintain_localization().unwrap();
            match vm
                .step(
                    &mut session,
                    SessionBudget::RetryMinimum(SliceBudget::default()),
                    || 0,
                )
                .unwrap()
            {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    vm.dispatch(
                        &mut session,
                        ticket,
                        "en",
                        reel_engine::aside::NativeBudget::Exact(64),
                    )
                    .unwrap();
                }
                SessionStep::Vm(RunOutcome::Returned(_)) => {
                    vm.discard_delivery(&mut session).unwrap();
                }
                SessionStep::Vm(RunOutcome::Fault(fault)) => panic!(
                    "source translation fault {:?}",
                    vm.vm().delivery_fault(fault).unwrap()
                ),
                SessionStep::Vm(RunOutcome::ResourceBlocked(error)) => {
                    panic!("source translation resource {error:?}")
                }
                SessionStep::Idle if !vm.has_runnable_tasks() && !vm.localization_pending() => {
                    break;
                }
                _ => {}
            }
        }
    }
    vm.sync_stage(&session).unwrap();
    vm
}
#[test]
fn imported_translations_retain_their_author_module_and_alias_identity() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(
        root,
        "main.aside",
        r#"use game::story;use game::ui::menu;fn main(){localization.set_locale("en");stage.replace(column{ text(story.greeting());text(menu.title()); });}"#,
    );
    write(
        root,
        "story.aside",
        r#"use reel::prelude::tr as translated;pub fn greeting(){translated("雨停了。",{en:"The rain stopped."})}"#,
    );
    write(
        root,
        "ui/menu.aside",
        r#"pub fn title(){tr("打开 { 文件 }",{en:"Open { file }"})}fn custom(){let tr=|value|value;tr("ordinary function");}"#,
    );
    let loaded = project::load(root).unwrap_or_else(|e| panic!("{:?}", e.failure));
    let checked = loaded.check().unwrap();
    assert_eq!(checked.catalogs.messages.len(), 2);
    for suffix in ["story.aside", "ui/menu.aside"] {
        assert!(
            checked
                .catalogs
                .messages
                .iter()
                .any(|message| message.module.ends_with(suffix)
                    && message.span.start() < message.span.end())
        );
    }
    assert!(!checked.catalogs.catalogs["en"].contains("ordinary function"));
    let vm = run(checked);
    for expected in ["The rain stopped.", "Open { file }"] {
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text(expected.into())),
            "missing {expected}"
        );
    }
    assert!(!root.join("build").exists());
    assert!(!root.join("translations").exists());
}
#[test]
fn inline_interpolation_and_literal_paragraphs_share_the_source_loader() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(
        root,
        "main.aside",
        r#"fn main(){
        localization.set_locale("en");let name="Mio";let count=3;
        let greeting=tr(f"你好{name}，{count}封信。",{en:f"Hello {name}, {count} letters."});
        let literal=tr("原文",{en:"  {quoted}\n\n.second\t "});
        stage.replace(column{text(greeting);text(literal);});
    }"#,
    );
    let loaded = project::load(root).unwrap_or_else(|e| panic!("{:?}", e.failure));
    let checked = loaded.check().unwrap();
    let vm = run(checked);
    assert!(vm.stage().nodes().any(|node|matches!(&node.kind,NodeKind::Text(text) if text.replace(['\u{2068}','\u{2069}'],"")=="Hello Mio, 3 letters.")));
    assert!(
        vm.stage()
            .nodes()
            .any(|node| node.kind == NodeKind::Text("  {quoted}\n\n.second\t ".into()))
    );
}
#[test]
fn source_translation_failure_preserves_the_previous_package() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(root, "main.aside", r#"fn main(){tr("你好",{en:"Hello"});}"#);
    let command = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_reel"))
            .current_dir(root)
            .args(args)
            .output()
            .unwrap()
    };
    let built = command(&["build", "--format", "json"]);
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stdout)
    );
    let package = root.join("build/debug/game.asb");
    let bytes = fs::read(&package).unwrap();
    write(root, "main.aside", r#"fn main(){tr("你好",{en:42});}"#);
    for action in ["check", "build"] {
        let output = command(&[action, "--format", "json"]);
        assert!(!output.status.success());
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(!report["diagnostics"].as_array().unwrap().is_empty());
        assert_eq!(fs::read(&package).unwrap(), bytes);
    }
}
