use aside_compiler::{DiagnosticCode, EntryPoint, ParseOptions, parse_with_options};
use std::process::Command;
#[test]
fn deep_else_chain_exits_in_a_separate_process() {
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "deep_child", "--nocapture"])
        .env("ASIDE_DEEP_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "child {:?}: {}",
        child.status,
        String::from_utf8_lossy(&child.stderr)
    );
}
#[test]
fn deep_child() {
    if std::env::var_os("ASIDE_DEEP_CHILD").is_none() {
        return;
    }
    let source = "if true {} else ".repeat(50_000) + "if true {}";
    let parsed = parse_with_options(
        &source,
        ParseOptions {
            entry: EntryPoint::Expression,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(parsed.exhausted());
    assert!(
        parsed
            .diagnostics()
            .iter()
            .any(|d| d.code == DiagnosticCode::BudgetExceeded)
    );
    assert_eq!(parsed.syntax().text().to_string(), source);
    drop(parsed); // dropping and traversing the bounded error tree must also be safe
}
