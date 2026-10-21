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
        fs::create_dir_all(dir.join("src")).unwrap();
        Self { dir }
    }
    fn run(&self, source: &[u8], format: &str) -> Output {
        fs::write(self.dir.join("src/main.aside"), source).unwrap();
        Command::new(env!("CARGO_BIN_EXE_reel"))
            .current_dir(&self.dir)
            .args(["check", "--format", format])
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
    let output = case.run(b"fn main() { loop {} }", "json");
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
    assert_eq!(report["entry"]["kind"], "project-initializer");
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
    assert_eq!(report["source"], "src/main.aside");
    assert_eq!(report["diagnostics"][0]["code"], "ASS002");
    assert_eq!(report["diagnostics"][0]["range"]["start"]["line"], 1);
    let output = case.run(source.as_bytes(), "human");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("src/main.aside:2:1"), "{diagnostic}");
    assert!(diagnostic.contains("missing"));
}

#[test]
fn imports_natives_and_unsupported_lowering_fail_honestly() {
    let case = Case::new();
    for (source, stage) in [
        ("use reel::story;", "analyze"),
        ("narrator(\"hello\");", "analyze"),
        ("fn main() { let [a]=[1]; a }", "lower"),
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
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["stage"], "input");
    assert_eq!(report["diagnostics"][0]["range"]["byte_start"], 0);
    assert_eq!(report["diagnostics"][0]["range"]["byte_end"], 1);
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
        vec!["check", "--bad", "--format=json"],
        vec!["check", "src/main.aside", "extra.aside", "--format", "json"],
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
fn source_input_rejects_directories() {
    let case = Case::new();
    assert_eq!(
        reel_cli::read_source(&case.dir).unwrap_err().code,
        "REEL-INPUT-FILE"
    );
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

#[test]
fn option_terminator_help_and_duplicate_options() {
    let case = Case::new();
    fs::write(case.dir.join("-source.aside"), "let value = 1;").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_reel"))
        .current_dir(&case.dir)
        .args(["check", "--help"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(env!("CARGO_BIN_EXE_reel"))
        .current_dir(&case.dir)
        .args(["check", "--", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("unexpected argument '--format'")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_reel"))
        .current_dir(&case.dir)
        .args(["check", "--format", "json", "--format", "human"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let report = serde_json::from_slice::<Value>(&output.stdout).unwrap();
    assert_eq!(report["stage"], "usage");
    assert!(
        report["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("cannot be used multiple times")
    );
}

#[test]
fn source_larger_than_old_cli_and_string_caps_passes_the_shared_pipeline() {
    let case = Case::new();
    let source = format!(
        "//{}\nuse reel::prelude::*; fn main() {{ let item={{next:null}}; item.next=item; let sum={}; let selected=item{}; stage.replace({}text(\"nested scene\");{}); }}",
        "x".repeat(8 * 1024 * 1024 + 1),
        vec!["1"; 256].join("+"),
        ".next".repeat(256),
        "column() {".repeat(24),
        "};".repeat(23) + "}"
    );
    let output = case.run(source.as_bytes(), "json");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "verified");
}

#[cfg(unix)]
#[test]
fn rejects_non_utf8_paths_and_special_files_without_opening_them() {
    use std::os::unix::{ffi::OsStringExt, net::UnixListener};
    let case = Case::new();
    let path = case.dir.join(std::ffi::OsString::from_vec(vec![0xff]));
    fs::write(&path, "let value = 1;").unwrap();
    assert_eq!(reel_cli::read_source(&path).unwrap(), "let value = 1;");
    let socket = case.dir.join("socket.aside");
    let _listener = UnixListener::bind(&socket).unwrap();
    assert_eq!(
        reel_cli::read_source(&socket).unwrap_err().code,
        "REEL-INPUT-FILE"
    );
}
