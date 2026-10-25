//! Exercise the author workflow from project directories without starting a window.
use reel_cli::{BuildProfile, build_export, distribution, project};
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn write(root: &Path, path: &str, value: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, value).unwrap();
}

fn command(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reel"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

fn temporary() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/reel-artifacts/project-workflow-tests");
    fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn project_discovery_fixes_entry_and_source_root_from_nested_directories() {
    let temporary = temporary();
    let root = temporary.path();
    write(
        root,
        "src/main.aside",
        "use game::utility::answer; fn main() { answer() }",
    );
    write(root, "src/utility.aside", "pub fn answer() { 42 }");
    write(root, "src/unused.aside", "invalid unused source");
    write(root, "src/ui/preview.aside", "fn main() { 7 }");
    write(root, "resources/images/sample.bin", "asset");
    let mut progress = Vec::new();
    let loaded =
        project::load_with_progress(&root.join("src/ui"), &mut |event| progress.push(event))
            .unwrap_or_else(|error| panic!("{:?}", error.failure));
    let checked = loaded
        .check_with_progress(&mut |event| progress.push(event))
        .unwrap();
    use aside_compiler::ProgressStage::{Analyze, Discover, Lower, Parse, Verify};
    let mut stages = Vec::new();
    for event in &progress {
        if stages.last() != Some(&event.stage) {
            stages.push(event.stage);
        }
        if event.total != 0 {
            assert!(event.completed <= event.total);
        }
    }
    assert_eq!(stages, [Discover, Parse, Analyze, Lower, Verify]);
    for stage in [Discover, Parse, Analyze, Lower] {
        let last = progress.iter().rfind(|event| event.stage == stage).unwrap();
        assert_eq!((last.completed, last.total), (2, 2));
    }
    assert_eq!(
        progress
            .last()
            .map(|event| (event.stage, event.completed, event.total)),
        Some((Verify, 1, 1))
    );
    assert_eq!(loaded.project_root, root.canonicalize().unwrap());
    assert_eq!(loaded.root, root.join("src").canonicalize().unwrap());
    assert_eq!(loaded.files.len(), 2);
    assert_eq!(
        loaded.assets(),
        root.join("resources").canonicalize().unwrap()
    );
    assert!(project::locate(&root.join("src/ui/preview.aside")).is_err());
    let report = success(command(
        &root.join("src/ui"),
        &["check", "--format", "json"],
    ));
    assert_eq!(report["status"], "verified");
    assert_eq!(
        report["encoded_bytes"],
        checked.compilation.program.stats().encoded_bytes
    );
    assert!(!root.join("build").exists());
    success(command(
        &root.join("src/ui"),
        &["build", "--format", "json"],
    ));
    assert!(root.join("build/debug/game.asb").is_file());
    assert!(!root.join("src/ui/build").exists());
    for action in ["check", "build", "export", "dev"] {
        for extra in [
            vec!["src/ui/preview.aside"],
            vec!["--entry", "main::main"],
            vec!["--module", "library=src"],
        ] {
            let result = command(root, &[vec![action], extra].concat());
            assert_eq!(result.status.code(), Some(2));
        }
    }
}

#[test]
fn root_main_is_not_a_project_and_check_validates_the_fixed_main() {
    let temporary = temporary();
    let root = temporary.path();
    write(root, "main.aside", "fn main() { 1 }");
    assert!(project::find(root).unwrap().is_none());
    assert_eq!(
        command(root, &["check", "--format", "json"]).status.code(),
        Some(2)
    );
    assert!(!root.join("build").exists());
    for source in ["fn other() { 1 }", "fn main(required) { required }"] {
        write(root, "src/main.aside", source);
        let output = command(root, &["check", "--format", "json"]);
        assert_eq!(output.status.code(), Some(1));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["stage"], "entry");
        assert!(!root.join("build").exists());
    }
}

#[test]
fn resource_preparation_and_export_share_rules_and_report_content_cache_hits() {
    let temporary = temporary();
    let root = temporary.path();
    write(root, "src/main.aside", "fn main() { 1 }");
    fs::create_dir_all(root.join("resources/ui")).unwrap();
    image::RgbaImage::from_pixel(13, 7, image::Rgba([80, 120, 160, 255]))
        .save(root.join("resources/ui/panel.png"))
        .unwrap();
    write(
        root,
        "resources.toml",
        "[[targets.vita]]\nmatch = 'ui/**'\nconvert = 'texture'\nscale = 0.5\nformat = 'rgba8'\n",
    );
    let first = success(command(
        root,
        &[
            "prepare-resources",
            "--resource-target",
            "vita",
            "--format",
            "json",
        ],
    ));
    assert_eq!(first["status"], "prepared");
    assert_eq!(first["report"]["converted"], 1);
    let next = success(command(
        root,
        &[
            "prepare-resources",
            "--resource-target",
            "vita",
            "--memory-bytes",
            "1",
            "--format",
            "json",
        ],
    ));
    assert_eq!(next["report"]["converted"], 0);
    assert_eq!(next["report"]["reused"], 1);
    success(command(
        root,
        &["export", "--resource-target", "vita", "--format", "json"],
    ));
    let output = fs::read(root.join("build/export/vita/debug/resources/ui/panel.png")).unwrap();
    let (header, _) = reel_core::texture::TextureHeader::parse(&output).unwrap();
    assert_eq!(header.descriptor().layout.logical_size, [13, 7]);
    assert_eq!(header.descriptor().layout.content_size, [6, 3]);
    assert!(
        fs::read(root.join("resources/ui/panel.png"))
            .unwrap()
            .starts_with(b"\x89PNG")
    );
    assert_eq!(
        command(root, &["prepare-resources", "--memory-bytes", "0"])
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn default_builds_replace_after_success_and_explicit_outputs_keep_overwrite_choice() {
    let temporary = temporary();
    let root = temporary.path();
    write(root, "src/main.aside", "fn main() { 1 }");
    success(command(root, &["build", "--format", "json"]));
    let output = root.join("build/debug/game.asb");
    let first = fs::read(&output).unwrap();
    write(root, "src/main.aside", "fn main() { 2 }");
    success(command(root, &["build", "--format", "json"]));
    let second = fs::read(&output).unwrap();
    assert_ne!(first, second);
    write(root, "src/main.aside", "fn main() { invalid_name() }");
    assert!(
        !command(root, &["build", "--format", "json"])
            .status
            .success()
    );
    assert_eq!(fs::read(&output).unwrap(), second);
    write(root, "src/main.aside", "fn main() { 3 }");
    let release = success(command(root, &["build", "--release", "--format", "json"]));
    assert_eq!(release["artifact"]["profile"], "release");
    assert_eq!(release["artifact"]["source_included"], false);
    assert!(root.join("build/release/game.asb").is_file());
    success(command(
        root,
        &["build", "--output", "selected.asb", "--format", "json"],
    ));
    let selected = fs::read(root.join("selected.asb")).unwrap();
    write(root, "src/main.aside", "fn main() { 4 }");
    assert!(
        !command(
            root,
            &["build", "--output", "selected.asb", "--format", "json"]
        )
        .status
        .success()
    );
    assert_eq!(fs::read(root.join("selected.asb")).unwrap(), selected);
    success(command(
        root,
        &[
            "build",
            "--output",
            "selected.asb",
            "--overwrite",
            "--format",
            "json",
        ],
    ));
    assert_ne!(fs::read(root.join("selected.asb")).unwrap(), selected);
}

#[test]
fn default_export_preserves_inputs_and_explicit_mounts_select_the_complete_set() {
    let temporary = temporary();
    let root = temporary.path();
    let source = "fn main() { 1 }";
    write(root, "src/main.aside", source);
    write(root, "resources/images/sample.bin", "original resource");
    write(root, "catalogs/en.ftl", "message = translated");
    success(command(root, &["export", "--format", "json"]));
    let output = root
        .join("build/export")
        .join(std::env::consts::OS)
        .join("debug");
    assert_eq!(
        fs::read_to_string(output.join("resources/images/sample.bin")).unwrap(),
        "original resource"
    );
    assert_eq!(
        fs::read_to_string(output.join("src/main.aside")).unwrap(),
        source
    );
    assert!(output.join("resources").is_dir());
    let manifest = reel_player::distribution::read_manifest(&output.join("game.asb")).unwrap();
    assert_eq!(
        manifest.loose,
        Some(reel_package::distribution::LooseResources::Directory(
            "resources".into()
        ))
    );
    write(root, "resources/images/sample.bin", "updated resource");
    success(command(root, &["export", "--format", "json"]));
    assert_eq!(
        fs::read_to_string(output.join("resources/images/sample.bin")).unwrap(),
        "updated resource"
    );
    assert_eq!(
        fs::read_to_string(root.join("src/main.aside")).unwrap(),
        source
    );
    assert_eq!(
        fs::read_to_string(root.join("resources/images/sample.bin")).unwrap(),
        "updated resource"
    );
    success(command(
        root,
        &[
            "export",
            "--output",
            "build/text-only",
            "--text",
            "locales=catalogs",
            "--format",
            "json",
        ],
    ));
    assert!(
        root.join("build/text-only/resources/locales/en.ftl")
            .is_file()
    );
    assert!(
        !root
            .join("build/text-only/resources/images/sample.bin")
            .exists()
    );
    success(command(
        root,
        &[
            "export",
            "--output",
            "build/root-mount",
            "--resource",
            ".=resources",
            "--format",
            "json",
        ],
    ));
    assert!(
        root.join("build/root-mount/resources/images/sample.bin")
            .is_file()
    );
    assert!(
        !command(
            root,
            &[
                "export",
                "--output",
                "build/root-mount",
                "--resource",
                ".=resources",
                "--format",
                "json"
            ]
        )
        .status
        .success()
    );
    write(root, "resources/game.asb", "ordinary resource");
    success(command(root, &["export", "--format", "json"]));
    reel_player::distribution::load(&output.join("game.asb"), None).unwrap();
    assert_eq!(
        fs::read_to_string(output.join("resources/game.asb")).unwrap(),
        "ordinary resource"
    );
    write(root, "resources/@vita/images/sample.bin", "vita resource");
    write(root, "resources/@vita/target-only.bin", "vita only");
    write(root, "resources/@other/excluded.bin", "other target");
    success(command(
        root,
        &["export", "--resource-target", "vita", "--format", "json"],
    ));
    assert_eq!(
        fs::read_to_string(output.join("resources/images/sample.bin")).unwrap(),
        "vita resource"
    );
    assert_eq!(
        fs::read_to_string(output.join("resources/target-only.bin")).unwrap(),
        "vita only"
    );
    assert!(!output.join("resources/@vita").exists());
    assert!(!output.join("resources/@other").exists());
    assert!(!output.join("resources/excluded.bin").exists());
    assert_eq!(
        fs::read_to_string(root.join("resources/images/sample.bin")).unwrap(),
        "updated resource"
    );
}

#[test]
fn cleaning_resources_keeps_exports_source_and_saves() {
    let temporary = temporary();
    let root = temporary.path();
    for path in [
        "src/main.aside",
        "resources/font.ttf",
        "saves/progress",
        "build/resources/objects/data",
        "build/export/vita/release/game.asb",
    ] {
        write(root, path, "retained content");
    }
    assert!(command(root, &["clean", "--resources"]).status.success());
    assert!(!root.join("build/resources").exists());
    assert!(root.join("build/export/vita/release/game.asb").is_file());
    assert!(command(root, &["clean"]).status.success());
    assert!(!root.join("build").exists());
    for path in ["src/main.aside", "resources/font.ttf", "saves/progress"] {
        assert_eq!(
            fs::read_to_string(root.join(path)).unwrap(),
            "retained content"
        );
    }
}

#[test]
fn export_allows_project_build_but_protects_imported_sources_and_resource_trees() {
    let temporary = temporary();
    let root = temporary.path();
    write(
        root,
        "src/main.aside",
        "use game::library::part::value; fn main() { value() }",
    );
    write(root, "src/library/part.aside", "pub fn value() { 1 }");
    write(root, "resources/image.bin", "resource");
    let loaded = project::load(root).unwrap_or_else(|error| panic!("{:?}", error.failure));
    let checked = loaded.check().unwrap();
    let player = root.join("player");
    fs::write(&player, "player fixture").unwrap();
    let mut options = distribution::ExportOptions {
        output: root.join("build/export/release"),
        overwrite: true,
        resources: vec![
            distribution::Mount::parse(&format!(".={}", root.join("resources").display())).unwrap(),
        ],
        text: vec![],
        resource_target: std::env::consts::OS.into(),
        pack_bytes: 4096.try_into().unwrap(),
        runtime: Some(reel_cli::distribution::Runtime::Executable(player)),
    };
    let artifact = build_export(&checked, "main::main", BuildProfile::Release).unwrap();
    distribution::export(&loaded, &artifact, &options).unwrap();
    assert!(options.output.join("resources.rpk").is_file());
    assert_eq!(
        fs::read_to_string(options.output.join(distribution::exported_player_name())).unwrap(),
        "player fixture"
    );
    for (directory, message) in [
        ("src", "input source"),
        ("src/library", "input source"),
        ("resources/subdir", "resource directory"),
    ] {
        options.output = root.join(directory);
        let error = distribution::export(&loaded, &artifact, &options)
            .err()
            .unwrap();
        assert!(error.to_string().contains(message), "{error}");
    }
    assert!(root.join("src/main.aside").is_file());
    assert!(root.join("src/library/part.aside").is_file());
    assert_eq!(
        fs::read_to_string(root.join("resources/image.bin")).unwrap(),
        "resource"
    );
}

#[test]
fn run_defaults_to_project_profile_and_rejects_profile_with_an_explicit_asb() {
    let temporary = temporary();
    let root = temporary.path();
    write(root, "src/main.aside", "fn main() { 1 }");
    for args in [vec!["run"], vec!["run", "--release"]] {
        let output = command(root, &args);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        let profile = if args.len() > 1 { "release" } else { "debug" };
        assert!(
            error
                .replace('\\', "/")
                .contains(&format!("build/{profile}/game.asb")),
            "{error}"
        );
    }
    let output = command(root, &["run", "chosen.asb", "--release"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
}

#[test]
fn project_commands_prepare_inline_languages_and_preserve_older_builds() {
    let temporary = temporary();
    let root = temporary.path();
    let script = |body: &str| {
        format!(r#"fn main() {{localization.set_locale("en");tr("source",{{en:"{body}"}})}}"#)
    };
    write(root, "src/main.aside", &script("First translation"));
    success(command(root, &["check", "--format", "json"]));
    assert!(!root.join("build").exists());
    let report = success(command(root, &["build", "--format", "json"]));
    let package = root.join("build/debug/game.asb");
    let first = fs::read(&package).unwrap();
    assert_eq!(report["artifact"]["blake3"], reel_package::digest(&first));
    let manifest = reel_player::distribution::read_manifest(&package).unwrap();
    assert_eq!(manifest.catalogs["en"], "__reel/catalogs/en.ftl");
    assert_eq!(
        manifest.loose,
        Some(reel_package::distribution::LooseResources::Project)
    );
    let first_archive = package.parent().unwrap().join(manifest.text.unwrap());
    assert!(first_archive.is_file());
    let old = package.with_file_name("old.asb");
    fs::write(&old, &first).unwrap();
    write(root, "src/main.aside", &script("Second translation"));
    let report = success(command(root, &["build", "--format", "json"]));
    let second = fs::read(&package).unwrap();
    assert_eq!(report["artifact"]["blake3"], reel_package::digest(&second));
    let current = reel_player::distribution::load(&package, None).unwrap();
    let previous = reel_player::distribution::load(&old, None).unwrap();
    assert_eq!(previous.packages, [first_archive.canonicalize().unwrap()]);
    assert_ne!(current.packages, previous.packages);
    assert_eq!(current.catalogs, previous.catalogs);
    assert!(first_archive.is_file());
    success(command(root, &["build", "--release", "--format", "json"]));
    let release =
        reel_player::distribution::load(&root.join("build/release/game.asb"), None).unwrap();
    assert_eq!(release.catalogs, current.catalogs);
    assert_eq!(release.packages.len(), 1);
    assert!(
        release
            .package
            .metadata()
            .modules
            .iter()
            .all(|module| module.debug.is_none())
    );
    write(
        root,
        "src/main.aside",
        r#"fn main(){tr("source",{en:42});}"#,
    );
    for action in ["check", "build"] {
        let result = command(root, &[action, "--format", "json"]);
        assert!(!result.status.success());
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert!(!report["diagnostics"].as_array().unwrap().is_empty());
        assert_eq!(fs::read(&package).unwrap(), second);
        assert_eq!(fs::read(&old).unwrap(), first);
    }
}

mod projects {
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
            let path = self.dir.join(if path.ends_with(".aside") {
                PathBuf::from("src").join(path)
            } else {
                PathBuf::from(path)
            });
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
                match vm
                    .step(
                        &mut session,
                        reel_engine::aside::SessionBudget::Exact(SliceBudget::default()),
                        || 0,
                    )
                    .unwrap()
                {
                    SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                        vm.dispatch(
                            &mut session,
                            ticket,
                            "en",
                            reel_engine::aside::NativeBudget::Exact(8),
                        )
                        .unwrap();
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
    fn copied_source_aliases_share_module_state_and_code_runs_after_moving_the_artifact() {
        let case = Case::new();
        case.write(
        "library/data.aside",
        "pub use reel::prelude::text as label; use std::random; pub let model={value:0}; pub fn bump(){model.value+=random(seed: 42).int(1,2);}",
    );
        case.write("main.aside", "use game::library::data as a; use game::library::data as b; use game::main as entry; pub let project={}; fn main(){if entry.project!=project {throw \"entry identity\";}game.configure(title: \"窗\"+\"边\", width: 400*2, height: 600, fonts: [\"body.ttf\", \"cjk.ttf\"]); let label=text(\"Title\", font: \"serif\"); label.font=\"sans-serif, system-ui\"; if label.font!=\"sans-serif, system-ui\" {throw \"font\";} a.bump(); b.bump(); if a.model!=b.model {throw \"identity\";} if a.label!=text {throw \"native identity\";} a.model.value==2}");
        let loaded = reel_cli::project::load(&case.dir)
            .unwrap_or_else(|error| panic!("{:?}", error.failure));
        assert_eq!(loaded.sources.len(), 2);
        let checked = loaded.check().unwrap();
        let (result, settings) =
            execute(reel_cli::into_story(checked, reel_cli::project::ENTRY).unwrap());
        assert!(result);
        assert_eq!(settings.title, "窗边");
        assert_eq!(settings.canvas_size, Some([800, 600]));
        assert_eq!(settings.fonts, ["body.ttf", "cjk.ttf"]);
        let output = case.command(&["build", "--output", "project.asb", "--format", "json"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let destination = case.dir.join("moved");
        fs::create_dir_all(&destination).unwrap();
        fs::rename(case.dir.join("project.asb"), destination.join("story.asb")).unwrap();
        fs::remove_file(case.dir.join("src/main.aside")).unwrap();
        let bytes = fs::read(destination.join("story.asb")).unwrap();
        let package = reel_package::decode(&bytes, Default::default()).unwrap();
        assert_eq!(package.metadata().modules.len(), 2);
        let (result, settings) =
            execute(reel_player::distribution::story_from_package(package).unwrap());
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
        let output = case.command(&["check", "--format", "json"]);
        assert!(!output.status.success());
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["executed"], false);
        let diagnostics = report["diagnostics"].as_array().unwrap();
        let diagnostic = diagnostics
            .iter()
            .find(|d| d["source"] == "src/ui.aside")
            .unwrap_or_else(|| panic!("imported source diagnostic: {report}"));
        assert_eq!(diagnostic["range"]["start"]["line"], 2);
        fs::remove_file(case.dir.join("src/ui.aside")).unwrap();
        let output = case.command(&["check", "--format", "json"]);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["stage"], "resolve");
        assert_eq!(report["diagnostics"][0]["source"], "src/main.aside");
        assert!(!case.dir.join("project.asb").exists());
        case.write("main.aside", "use game::ui::{panel}; fn main(){} ");
        case.write("ui.aside", "pub fn panel(required){required}");
        let loaded = reel_cli::project::load(&case.dir)
            .unwrap_or_else(|error| panic!("{:?}", error.failure));
        let checked = loaded.check().unwrap();
        let failure = reel_cli::build(&checked, Some("game::ui::panel"))
            .err()
            .unwrap();
        assert_eq!(failure.stage, "entry");
        assert_eq!(
            failure.diagnostics[0].source.as_deref(),
            Some("src/ui.aside")
        );
        assert!(!case.dir.join("project.asb").exists());
    }

    #[test]
    fn runnable_project_example_checks_and_builds_the_complete_source_graph() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/author_project");
        let loaded =
            reel_cli::project::load(&path).unwrap_or_else(|error| panic!("{:?}", error.failure));
        assert_eq!(loaded.sources.len(), 4);
        let checked = loaded.check().unwrap();
        let artifact = reel_cli::build(&checked, Some(reel_cli::project::ENTRY)).unwrap();
        let package = reel_package::decode(&artifact.bytes, Default::default()).unwrap();
        assert_eq!(package.metadata().modules.len(), 4);
        assert!(package.metadata().game_entry.is_some());
    }
}
