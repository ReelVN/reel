//! Exercise the author workflow from project directories without starting a window.
use reel_cli::{BuildProfile, build_export, distribution, project};
use serde_json::Value;
use std::{
    collections::BTreeMap,
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
fn directory_and_explicit_entries_share_project_outputs_with_distinct_module_roots() {
    let temporary = temporary();
    let root = temporary.path();
    write(
        root,
        "src/main.aside",
        "use game::utility::answer; fn main() { answer() }",
    );
    write(root, "src/utility.aside", "pub fn answer() { 42 }");
    write(root, "src/unused.aside", "invalid unused source");
    write(root, "main.aside", "invalid lower-priority entry");
    write(root, "src/ui/preview.aside", "fn main() { 7 }");
    write(root, "resources/images/sample.bin", "asset");
    let loaded = project::load(root).unwrap_or_else(|error| panic!("{:?}", error.failure));
    assert_eq!(loaded.project_root, root.canonicalize().unwrap());
    assert_eq!(loaded.root, root.join("src").canonicalize().unwrap());
    assert_eq!(loaded.files.len(), 2);
    assert_eq!(
        loaded.assets(),
        root.join("resources").canonicalize().unwrap()
    );
    let preview = project::load(&root.join("src/ui/preview.aside"))
        .unwrap_or_else(|error| panic!("{:?}", error.failure));
    assert_eq!(preview.project_root, loaded.project_root);
    assert_eq!(preview.root, root.join("src/ui").canonicalize().unwrap());
    assert_eq!(preview.files.len(), 1);
    let located = project::locate(&root.join("src/ui")).unwrap();
    assert_eq!(located.root, loaded.project_root);
    assert_eq!(
        located.input,
        root.join("src/main.aside").canonicalize().unwrap()
    );
    let report = success(command(
        &root.join("src/ui"),
        &["check", "--format", "json"],
    ));
    assert_eq!(report["status"], "verified");
    assert!(!root.join("build").exists());
    let report = success(command(
        root,
        &["build", "src/ui/preview.aside", "--format", "json"],
    ));
    assert_eq!(report["artifact"]["profile"], "debug");
    assert!(root.join("build/debug/game.asb").is_file());
    assert!(!root.join("src/ui/build").exists());
}

#[test]
fn default_builds_replace_after_success_and_explicit_outputs_keep_overwrite_choice() {
    let temporary = temporary();
    let root = temporary.path();
    write(root, "main.aside", "fn main() { 1 }");
    success(command(root, &["build", "--format", "json"]));
    let output = root.join("build/debug/game.asb");
    let first = fs::read(&output).unwrap();
    write(root, "main.aside", "fn main() { 2 }");
    success(command(root, &["build", "--format", "json"]));
    let second = fs::read(&output).unwrap();
    assert_ne!(first, second);
    write(root, "main.aside", "fn main() { invalid_name() }");
    assert!(
        !command(root, &["build", "--format", "json"])
            .status
            .success()
    );
    assert_eq!(fs::read(&output).unwrap(), second);
    write(root, "main.aside", "fn main() { 3 }");
    let release = success(command(root, &["build", "--release", "--format", "json"]));
    assert_eq!(release["artifact"]["profile"], "release");
    assert_eq!(release["artifact"]["source_included"], false);
    assert!(root.join("build/release/game.asb").is_file());
    success(command(
        root,
        &["build", "--output", "selected.asb", "--format", "json"],
    ));
    let selected = fs::read(root.join("selected.asb")).unwrap();
    write(root, "main.aside", "fn main() { 4 }");
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
        fs::read_to_string(output.join("images/sample.bin")).unwrap(),
        "original resource"
    );
    assert_eq!(
        fs::read_to_string(output.join("script/main.aside")).unwrap(),
        source
    );
    assert!(!output.join("resources").exists());
    let manifest = distribution::read_manifest(&output.join("game.asb")).unwrap();
    assert_eq!(manifest.source.unwrap().entry, "script/main.aside");
    write(root, "resources/images/sample.bin", "updated resource");
    success(command(root, &["export", "--format", "json"]));
    assert_eq!(
        fs::read_to_string(output.join("images/sample.bin")).unwrap(),
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
    assert!(root.join("build/text-only/locales/en.ftl").is_file());
    assert!(!root.join("build/text-only/images/sample.bin").exists());
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
    assert!(root.join("build/root-mount/images/sample.bin").is_file());
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
    let previous = fs::read(output.join("game.asb")).unwrap();
    write(root, "resources/game.asb", "colliding resource");
    assert!(
        !command(root, &["export", "--format", "json"])
            .status
            .success()
    );
    assert_eq!(fs::read(output.join("game.asb")).unwrap(), previous);
    assert_eq!(
        fs::read_to_string(root.join("resources/game.asb")).unwrap(),
        "colliding resource"
    );
}

#[test]
fn export_allows_project_build_but_protects_imported_sources_and_resource_trees() {
    let temporary = temporary();
    let root = temporary.path();
    write(
        root,
        "src/main.aside",
        "use shared::part::value; fn main() { value() }",
    );
    write(root, "library/part.aside", "pub fn value() { 1 }");
    write(root, "resources/image.bin", "resource");
    let modules = BTreeMap::from([("shared".into(), root.join("library"))]);
    let loaded = project::load_with_modules(root, &modules)
        .unwrap_or_else(|error| panic!("{:?}", error.failure));
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
        modules,
        entry: "main::main".into(),
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
        ("library", "input source"),
        ("resources/subdir", "resource directory"),
    ] {
        options.output = root.join(directory);
        let error = distribution::export(&loaded, &artifact, &options)
            .err()
            .unwrap();
        assert!(error.to_string().contains(message), "{error}");
    }
    assert!(root.join("src/main.aside").is_file());
    assert!(root.join("library/part.aside").is_file());
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
