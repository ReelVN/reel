//! Module catalogs keep runtime identities and publish only complete valid bundles.
use reel_cli::localization::{bundle, check, extract, extract_directory};
use reel_engine::localization::{Catalog, Localization, message_id};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

struct Case(PathBuf);
impl Case {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/reel-artifacts/localization-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, path: &str, text: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.0.join(path)).unwrap()
    }
    fn command(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_reel"))
            .current_dir(&self.0)
            .args(arguments)
            .output()
            .unwrap()
    }
}
fn succeeds(output: Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn extraction_mirrors_imported_modules_and_keeps_one_identity_with_all_call_sites() {
    let case = Case::new();
    case.write(
        "main.aside",
        r#"
use game::story::day_one;
use game::ui::menu;
fn main() { day_one.play(); menu.show(); tr("共享"); }
"#,
    );
    case.write(
        "story/day_one.aside",
        r#"
use reel::prelude::tr as translated;
pub fn play() { narrator(translated("共享")); narrator(translated("雨停了。", id: "opening")); }
"#,
    );
    case.write(
        "ui/menu.aside",
        r#"
pub fn show() { let _ = text(tr("共享", locale: "en")); let _ = text(tr("打开 { 文件 }")); }
fn custom() { let tr = |value| value; tr("not a translation"); }
"#,
    );
    let output = case.command(&["loc", "extract", ".", "--output-dir", "source"]);
    succeeds(output);
    let shared = message_id("共享");
    let main = case.read("source/main.ftl");
    assert_eq!(main.matches(&format!("{shared} =")).count(), 1);
    for reference in ["main.aside", "story/day_one.aside", "ui/menu.aside"] {
        assert!(main.contains(reference), "{main}");
    }
    assert!(!case.read("source/story/day_one.ftl").contains(&shared));
    assert!(!case.read("source/ui/menu.ftl").contains(&shared));
    assert!(
        !case
            .read("source/ui/menu.ftl")
            .contains("not a translation")
    );
    succeeds(case.command(&["loc", "bundle", "source", "--output", "dist/en.ftl"]));
    succeeds(case.command(&["loc", "check", "dist/en.ftl", "--locale", "en"]));
    let before = case.read("dist/en.ftl");
    succeeds(case.command(&[
        "loc",
        "extract",
        ".",
        "--output-dir",
        "source",
        "--overwrite",
    ]));
    succeeds(case.command(&[
        "loc",
        "bundle",
        "source",
        "--output",
        "dist/en.ftl",
        "--overwrite",
    ]));
    assert_eq!(before, case.read("dist/en.ftl"));
    let single = extract(&case.0, &case.0.join("single.ftl"), false).unwrap();
    assert_eq!((single.messages, single.occurrences), (3, 5));
    for text in [case.read("dist/en.ftl"), case.read("single.ftl")] {
        let mut localization = Localization::default();
        localization.install(Catalog::parse("en", "test", text).unwrap());
        assert_eq!(
            localization.format(&shared, "missing", None).unwrap(),
            "共享"
        );
        assert_eq!(
            localization.format("opening", "missing", None).unwrap(),
            "雨停了。"
        );
        assert_eq!(
            localization
                .format(&message_id("打开 { 文件 }"), "missing", None)
                .unwrap(),
            "打开 { 文件 }"
        );
    }
}

#[test]
fn extraction_reports_stale_catalogs_and_overwrites_only_explicit_outputs() {
    let case = Case::new();
    case.write(
        "main.aside",
        "use game::story; fn main() { story.play(); tr(\"Shared\"); }",
    );
    case.write("story.aside", "pub fn play() { tr(\"Shared\"); }");
    let target = case.0.join("source");
    extract_directory(&case.0, &target, false).unwrap();
    assert!(target.join("main.ftl").is_file());
    case.write("main.aside", "use game::story; fn main() { story.play(); }");
    let result = extract_directory(&case.0, &target, true).unwrap();
    assert_eq!(
        result.stale_catalogs,
        [target.join("main.ftl").canonicalize().unwrap()]
    );
    assert!(target.join("main.ftl").is_file());
    assert!(target.join("story.ftl").is_file());
    let before = case.read("source/story.ftl");
    case.write("source/translated.ftl", "message = Traduction\n");
    let result = extract_directory(&case.0, &target, true).unwrap();
    assert_eq!(result.stale_catalogs.len(), 2);
    assert_eq!(before, case.read("source/story.ftl"));
    assert_eq!(case.read("source/translated.ftl"), "message = Traduction\n");
    assert!(extract(&case.0, &target.join("translated.ftl"), false).is_err());
    extract(&case.0, &target.join("translated.ftl"), true).unwrap();
    assert!(
        case.read("source/translated.ftl")
            .contains(&message_id("Shared"))
    );
}

#[test]
fn bundles_resolve_cross_file_terms_messages_and_attributes_without_runtime_file_scans() {
    let case = Case::new();
    case.write(
        "en/common.ftl",
        "-brand = Reel\nname = Mio\n    .title = Captain\n",
    );
    case.write(
        "en/story/day_one.ftl",
        "welcome = Hello, { name.title } { name }! Welcome to { -brand }.\n",
    );
    case.write("en/notes.txt", "not a catalog");
    let output = case.0.join("dist/en.ftl");
    let result = bundle(&case.0.join("en"), &output, false).unwrap();
    assert_eq!((result.files, result.entries), (2, 3));
    check(&output, "en").unwrap();
    let text = fs::read_to_string(&output).unwrap();
    let mut localization = Localization::default();
    localization.install(Catalog::parse("en", "dist/en.ftl", text.clone()).unwrap());
    let rendered = localization.format("welcome", "missing", None).unwrap();
    assert_eq!(
        rendered.replace(['\u{2068}', '\u{2069}'], ""),
        "Hello, Captain Mio! Welcome to Reel."
    );
    assert!(bundle(&case.0.join("en"), &output, false).is_err());
    assert_eq!(fs::read_to_string(&output).unwrap(), text);
    bundle(&case.0.join("en"), &output, true).unwrap();
    assert_eq!(fs::read_to_string(&output).unwrap(), text);
    assert!(bundle(&case.0.join("en"), &case.0.join("en/combined.ftl"), true).is_err());
    assert!(!case.0.join("en/combined.ftl").exists());
}

#[test]
fn bundle_failure_preserves_output_and_reports_original_declarations() {
    let case = Case::new();
    case.write("dist/en.ftl", "existing = Keep this translation\n");
    let output = case.0.join("dist/en.ftl");
    for (first, duplicate, first_column, duplicate_column) in [
        ("message = First", "message = Second", 1, 1),
        ("-term = First", "-term = Second", 2, 2),
        ("-shared = First", "shared = Second", 2, 1),
    ] {
        case.write("en/a.ftl", &format!("# first declaration\n{first}\n"));
        case.write("en/story/b.ftl", &format!("# duplicate\n\n{duplicate}\n"));
        let error = bundle(&case.0.join("en"), &output, true).err().unwrap();
        assert!(
            error.contains(&format!("a.ftl:2:{first_column}")),
            "{error}"
        );
        assert!(
            error.contains(&format!("b.ftl:3:{duplicate_column}")),
            "{error}"
        );
        assert!(
            error.contains("first declaration") && error.contains("duplicate Fluent id"),
            "{error}"
        );
        assert_eq!(
            case.read("dist/en.ftl"),
            "existing = Keep this translation\n"
        );
    }
    case.write("en/story/b.ftl", "good = Valid\nbad = { @ }\n");
    let error = bundle(&case.0.join("en"), &output, true).err().unwrap();
    assert!(error.contains("b.ftl:2:"), "{error}");
    assert_eq!(
        case.read("dist/en.ftl"),
        "existing = Keep this translation\n"
    );
    assert_eq!(fs::read_dir(case.0.join("dist")).unwrap().count(), 1);
}

#[test]
fn invalid_cli_output_combinations_fail_without_creating_outputs() {
    let case = Case::new();
    for args in [
        vec![
            "loc",
            "extract",
            ".",
            "--output-dir",
            "source",
            "--output",
            "source.ftl",
        ],
        vec!["loc", "bundle", ".", "--output-dir", "source"],
        vec![
            "loc",
            "check",
            ".",
            "--locale",
            "en",
            "--output-dir",
            "source",
        ],
    ] {
        assert_eq!(case.command(&args).status.code(), Some(2));
    }
    assert_eq!(fs::read_dir(&case.0).unwrap().count(), 0);
}
