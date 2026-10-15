//! Real filesystem projects share compilation, ASB execution and source diagnostics.
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{RunOutcome, SessionStep, SessionVm, SliceBudget, VmLimits},
};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Case {
    dir: PathBuf,
}
impl Case {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/reel-artifacts/project-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }
    fn write(&self, path: &str, value: &str) {
        let path = self.dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }
    fn command(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_reel"))
            .current_dir(&self.dir)
            .args(args)
            .output()
            .unwrap()
    }
}
fn execute(
    story: reel_engine::aside::source::CompiledStory,
) -> (bool, reel_engine::aside::GameSettings) {
    let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
    let mut vm = SessionVm::load(&session, story.program, VmLimits::default()).unwrap();
    for function in [story.initializer, story.entry] {
        vm.start(&session, function).unwrap();
        for _ in 0..10_000 {
            match vm.step(&mut session, SliceBudget::default(), || 0).unwrap() {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    vm.dispatch(&mut session, ticket, "en", 8).unwrap();
                }
                SessionStep::Vm(RunOutcome::Returned(_)) => {
                    if function == story.entry {
                        return (
                            vm.retain_return(&session).unwrap().as_bool() == Some(true),
                            (**vm.game_settings()).clone(),
                        );
                    }
                    vm.discard_delivery(&mut session).unwrap();
                    break;
                }
                SessionStep::Vm(RunOutcome::Fault(id)) => {
                    panic!("{:?}", vm.vm().delivery_fault(id).unwrap())
                }
                _ => {}
            }
        }
    }
    panic!("project did not return");
}

#[test]
fn project_aliases_share_module_state_and_code_runs_after_moving_the_artifact() {
    let case = Case::new();
    case.write(
        "library/data.aside",
        "pub use reel::prelude::text as label; use std::random; pub let model={value:0}; pub fn bump(){model.value+=random(seed: 42).int(1,2);}",
    );
    case.write("main.aside", "use first::data as a; use second::data as b; use home::main as entry; pub let project={}; fn main(){if entry.project!=project {throw \"entry identity\";}game.configure(title: \"窗\"+\"边\", width: 400*2, height: 600, fonts: [\"body.ttf\", \"cjk.ttf\"]); let label=text(\"Title\", font: \"serif\"); label.font=\"sans-serif, system-ui\"; if label.font!=\"sans-serif, system-ui\" {throw \"font\";} a.bump(); b.bump(); if a.model!=b.model {throw \"identity\";} if a.label!=text {throw \"native identity\";} a.model.value==2}");
    let modules = std::collections::BTreeMap::from([
        ("first".into(), case.dir.join("library")),
        ("second".into(), case.dir.join("library")),
        ("home".into(), case.dir.clone()),
    ]);
    let loaded = reel_cli::project::load_with_modules(&case.dir, &modules)
        .unwrap_or_else(|error| panic!("{:?}", error.failure));
    assert_eq!(loaded.sources.len(), 2);
    let checked = loaded.check().unwrap();
    let (result, settings) = execute(reel_cli::into_story(checked, &loaded.entry(None)).unwrap());
    assert!(result);
    assert_eq!(settings.title, "窗边");
    assert_eq!(settings.canvas_size, Some([800, 600]));
    assert_eq!(settings.fonts, ["body.ttf", "cjk.ttf"]);
    let output = case.command(&[
        "build",
        ".",
        "--module",
        "first=library",
        "--module",
        "second=library",
        "--module",
        "home=.",
        "--output",
        "project.asb",
        "--format",
        "json",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let destination = case.dir.join("moved");
    fs::create_dir_all(&destination).unwrap();
    fs::rename(case.dir.join("project.asb"), destination.join("story.asb")).unwrap();
    fs::remove_file(case.dir.join("main.aside")).unwrap();
    let bytes = fs::read(destination.join("story.asb")).unwrap();
    let package = reel_package::decode(&bytes, Default::default()).unwrap();
    assert_eq!(package.metadata().modules.len(), 2);
    let (result, settings) = execute(reel_cli::story_from_package(package).unwrap());
    assert!(result);
    assert_eq!(settings.title, "窗边");
    assert_eq!(settings.fonts, ["body.ttf", "cjk.ttf"]);
}

#[test]
fn project_diagnostics_locate_imported_source_and_missing_import_without_executing() {
    let case = Case::new();
    case.write("main.aside", "use game::ui::{panel}; fn main(){panel();}");
    case.write(
        "ui.aside",
        "pub fn panel(){\n let text=\"字\";\n missing;\n}",
    );
    let output = case.command(&["check", "main.aside", "--format", "json"]);
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["executed"], false);
    let diagnostics = report["diagnostics"].as_array().unwrap();
    let diagnostic = diagnostics
        .iter()
        .find(|d| d["source"] == "ui.aside")
        .unwrap_or_else(|| panic!("imported source diagnostic: {report}"));
    assert_eq!(diagnostic["range"]["start"]["line"], 2);
    fs::remove_file(case.dir.join("ui.aside")).unwrap();
    let output = case.command(&["check", "main.aside", "--format", "json"]);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["stage"], "resolve");
    assert_eq!(report["diagnostics"][0]["source"], "main.aside");
    assert!(!case.dir.join("project.asb").exists());
    case.write("main.aside", "use game::ui::{panel}; fn main(){} ");
    case.write("ui.aside", "pub fn panel(required){required}");
    let output = case.command(&[
        "build",
        "main.aside",
        "--entry",
        "game::ui::panel",
        "--output",
        "project.asb",
        "--format",
        "json",
    ]);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["stage"], "entry");
    assert_eq!(report["diagnostics"][0]["source"], "ui.aside");
    assert!(!case.dir.join("project.asb").exists());
}

#[test]
fn runnable_project_example_checks_and_builds_the_complete_source_graph() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/author_project");
    let loaded =
        reel_cli::project::load(&path).unwrap_or_else(|error| panic!("{:?}", error.failure));
    assert_eq!(loaded.sources.len(), 4);
    let checked = loaded.check().unwrap();
    let artifact = reel_cli::build(&checked, Some(&loaded.entry(None))).unwrap();
    let package = reel_package::decode(&artifact.bytes, Default::default()).unwrap();
    assert_eq!(package.metadata().modules.len(), 4);
    assert!(package.metadata().game_entry.is_some());
}
