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
    let loaded =
        project::load(&root.join("src/ui")).unwrap_or_else(|error| panic!("{:?}", error.failure));
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
    let output = root.join("build/export/debug");
    assert_eq!(
        fs::read_to_string(output.join("resources/images/sample.bin")).unwrap(),
        "original resource"
    );
    assert_eq!(
        fs::read_to_string(output.join("src/main.aside")).unwrap(),
        source
    );
    assert!(output.join("resources").is_dir());
    let manifest = distribution::read_manifest(&output.join("game.asb")).unwrap();
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
    distribution::load(&output.join("game.asb")).unwrap();
    assert_eq!(
        fs::read_to_string(output.join("resources/game.asb")).unwrap(),
        "ordinary resource"
    );
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
        pack_bytes: 4096.try_into().unwrap(),
        player: Some(player),
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
fn project_commands_prepare_indexed_languages_and_preserve_older_builds() {
    let temporary = temporary();
    let root = temporary.path();
    write(
        root,
        "src/main.aside",
        "fn main() { localization.set_locale(\"en\"); tr(\"source\", id: \"greeting\") }",
    );
    write(
        root,
        "translations/en/ui.ftl",
        "greeting = First translation\n",
    );
    success(command(root, &["check", "--format", "json"]));
    assert!(!root.join("build").exists());
    let report = success(command(root, &["build", "--format", "json"]));
    let package = root.join("build/debug/game.asb");
    let first = fs::read(&package).unwrap();
    assert_eq!(report["artifact"]["blake3"], reel_package::digest(&first));
    let manifest = distribution::read_manifest(&package).unwrap();
    assert_eq!(manifest.catalogs["en"], "__reel/catalogs/en.ftl");
    assert_eq!(
        manifest.loose,
        Some(reel_package::distribution::LooseResources::Project)
    );
    let first_archive = package.parent().unwrap().join(manifest.text.unwrap());
    assert!(first_archive.is_file());
    let old = package.with_file_name("old.asb");
    fs::write(&old, &first).unwrap();
    write(
        root,
        "translations/en/ui.ftl",
        "greeting = Second translation\n",
    );
    let report = success(command(root, &["build", "--format", "json"]));
    let second = fs::read(&package).unwrap();
    assert_eq!(report["artifact"]["blake3"], reel_package::digest(&second));
    let current = distribution::load(&package).unwrap();
    let previous = distribution::load(&old).unwrap();
    assert_eq!(previous.packages, [first_archive.canonicalize().unwrap()]);
    assert_ne!(current.packages, previous.packages);
    assert_eq!(current.catalogs, previous.catalogs);
    assert!(first_archive.is_file());
    success(command(root, &["build", "--release", "--format", "json"]));
    let release = distribution::load(&root.join("build/release/game.asb")).unwrap();
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
        "translations/en/duplicate.ftl",
        "greeting = Duplicate\n",
    );
    for action in ["check", "build"] {
        let result = command(root, &[action, "--format", "json"]);
        assert!(!result.status.success());
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["stage"], "localization");
        assert!(
            report["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .contains("translations")
        );
        assert_eq!(fs::read(&package).unwrap(), second);
        assert_eq!(fs::read(&old).unwrap(), first);
    }
}
