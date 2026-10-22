//! Project formatting shares the compiler printer and publishes only validated batches.
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn temporary() -> tempfile::TempDir {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/reel-artifacts/format-tests");
    fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}

fn write(root: &Path, path: &str, bytes: impl AsRef<[u8]>) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn command(root: &Path, check: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_reel"));
    command.current_dir(root).arg("fmt");
    if check {
        command.arg("--check");
    }
    command.output().unwrap()
}

#[test]
fn formats_all_source_files_with_the_shared_printer_and_check_keeps_files_unchanged() {
    let temporary = temporary();
    let root = temporary.path();
    let main = "fn main(){let 中文=\"literal\\ntext\";missing(中文,extra:1);}";
    let unused = "/// Original documentation\nfn unused( x = 1 ){x+2}";
    write(root, "src/main.aside", main);
    write(root, "src/ui/unused.aside", unused);
    write(root, "resources/sample.aside", "resource bytes");
    write(root, "notes/sample.aside", "note bytes");
    let output = command(&root.join("src/ui"), true);
    assert_eq!(output.status.code(), Some(1));
    let listed = String::from_utf8(output.stdout).unwrap().replace('\\', "/");
    assert!(listed.contains("src/main.aside") && listed.contains("src/ui/unused.aside"));
    assert_eq!(
        fs::read_to_string(root.join("src/main.aside")).unwrap(),
        main
    );
    assert_eq!(
        fs::read_to_string(root.join("src/ui/unused.aside")).unwrap(),
        unused
    );
    let output = command(root, false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(root.join("src/main.aside")).unwrap(),
        aside_compiler::format(main).unwrap()
    );
    assert_eq!(
        fs::read_to_string(root.join("src/ui/unused.aside")).unwrap(),
        aside_compiler::format(unused).unwrap()
    );
    assert_eq!(
        fs::read_to_string(root.join("resources/sample.aside")).unwrap(),
        "resource bytes"
    );
    assert_eq!(
        fs::read_to_string(root.join("notes/sample.aside")).unwrap(),
        "note bytes"
    );
    assert!(command(root, true).status.success());
    assert!(
        String::from_utf8(command(root, false).stdout)
            .unwrap()
            .contains("Formatted 0 of 2")
    );
    assert!(!root.join("build").exists());
}

#[test]
fn syntax_or_utf8_failure_preserves_every_file_before_publication() {
    let temporary = temporary();
    let root = temporary.path();
    let source = "fn main(){1}";
    write(root, "src/main.aside", source);
    write(root, "src/z_unused.aside", "fn broken( {");
    let output = command(root, false);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .replace('\\', "/")
            .contains("src/z_unused.aside")
    );
    assert_eq!(
        fs::read_to_string(root.join("src/main.aside")).unwrap(),
        source
    );
    assert_eq!(
        fs::read_to_string(root.join("src/z_unused.aside")).unwrap(),
        "fn broken( {"
    );
    write(root, "src/z_unused.aside", [0xff]);
    assert_eq!(command(root, false).status.code(), Some(1));
    assert_eq!(
        fs::read_to_string(root.join("src/main.aside")).unwrap(),
        source
    );
    assert_eq!(fs::read(root.join("src/z_unused.aside")).unwrap(), [0xff]);
}
