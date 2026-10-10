//! These processes only invoke the static author CLI; no VM or script runs.
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
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/reel-artifacts/cli-tests");
        fs::create_dir_all(&root).unwrap();
        let dir = root.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        Self { dir }
    }
    fn run(&self, source: &[u8], format: &str) -> Output {
        fs::write(self.dir.join("input.aside"), source).unwrap();
        Command::new(env!("CARGO_BIN_EXE_reel"))
            .current_dir(&self.dir)
            .args(["check", "input.aside", "--format", format])
            .output()
            .unwrap()
    }
}
impl Drop for Case {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.dir).unwrap();
    }
}

#[test]
fn check_never_executes_an_infinite_loop_and_writes_no_artifacts() {
    let case = Case::new();
    let output = case.run(b"loop {}", "json");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "verified");
    assert_eq!(report["executed"], false);
    assert_eq!(report["runtime_admitted"], false);
    assert_eq!(report["release_package"], false);
    assert_eq!(report["entry"]["kind"], "module-initializer");
    assert_eq!(fs::read_dir(&case.dir).unwrap().count(), 1);
}

#[test]
fn machine_and_human_failures_locate_the_original_source() {
    let case = Case::new();
    let source = "let 中文 = 1;\r\nmissing();";
    let output = case.run(source.as_bytes(), "json");
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["stage"], "analyze");
    assert_eq!(report["source"], "input.aside");
    assert_eq!(report["diagnostics"][0]["code"], "ASS002");
    assert_eq!(report["diagnostics"][0]["range"]["start"]["line"], 1);
    let output = case.run(source.as_bytes(), "human");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("input.aside:2:1"), "{diagnostic}");
    assert!(diagnostic.contains("missing"));
}

#[test]
fn imports_natives_and_unsupported_lowering_fail_honestly() {
    let case = Case::new();
    for (source, stage) in [
        ("use reel::story;", "analyze"),
        ("narrator(\"hello\");", "analyze"),
        ("fn main() { [...[1]] }", "lower"),
    ] {
        let output = case.run(source.as_bytes(), "json");
        assert_eq!(output.status.code(), Some(1));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["stage"], stage);
        assert!(report["diagnostics"][0]["range"].is_object());
    }
}

#[test]
fn usage_io_and_invalid_utf8_are_machine_readable() {
    let case = Case::new();
    let output = case.run(&[0xff], "json");
    assert_eq!(output.status.code(), Some(2));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["stage"], "input");
    assert!(
        report["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("UTF-8")
    );
    for args in [
        vec!["check", "missing.aside", "--format", "json"],
        vec!["dev", "--format", "json"],
        vec!["check", "--bad", "--format", "json"],
        vec!["check", "input.aside", "extra.aside", "--format", "json"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_reel"))
            .current_dir(&case.dir)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(serde_json::from_slice::<Value>(&output.stdout).unwrap()["diagnostics"].is_array());
    }
}

#[test]
fn oversized_file_is_rejected_before_read_and_directories_are_rejected() {
    let case = Case::new();
    let path = case.dir.join("big.aside");
    fs::File::create(&path)
        .unwrap()
        .set_len(reel_cli::MAX_SOURCE_BYTES as u64 + 1)
        .unwrap();
    for path in [path.as_path(), case.dir.as_path()] {
        assert!(reel_cli::read_source(path).is_err());
    }
}

#[test]
fn compiler_related_locations_and_fixes_survive_json_rendering() {
    let case = Case::new();
    let output = case.run(b"let a = 1; let a = 2;", "json");
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let related = &report["diagnostics"][0]["related"];
    assert!(!related.as_array().unwrap().is_empty());
    assert!(related[0]["range"]["start"]["byte_column"].is_number());

    let output = case.run(b"let a = 1", "json");
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["fix"]["replacement"] == ";")
    );
}
