use aside_compiler::{DiagnosticCode, EntryPoint, ParseOptions, parse_with_options};
use std::process::Command;
#[path = "../../reel-dev/src/process.rs"]
#[allow(
    dead_code,
    reason = "shared subprocess helper also exposes exit-code conversion for tool observers"
)]
mod process;
const BUDGET_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

fn budget_child(command: &mut Command, timeout: std::time::Duration) -> process::Outcome {
    process::run(command, timeout, None).unwrap()
}

#[test]
fn deep_else_chain_exits_in_a_separate_process() {
    let child = budget_child(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "deep_child", "--nocapture"])
            .env("ASIDE_DEEP_CHILD", "1"),
        BUDGET_PROBE_TIMEOUT,
    );
    assert!(
        !child.timed_out,
        "deep parser child exceeded the original 15 second bound"
    );
    assert!(
        child.status.success(),
        "child {:?}: {}",
        child.status,
        String::from_utf8_lossy(&child.stderr)
    );
}

#[cfg(unix)]
#[test]
fn budget_probe_keeps_fifteen_second_policy_and_kills_a_timed_out_mock() {
    assert_eq!(BUDGET_PROBE_TIMEOUT, std::time::Duration::from_secs(15));
    let started = std::time::Instant::now();
    let child = budget_child(
        Command::new("/bin/sh").args([
            "-c",
            "printf 'budget-partial'; printf 'error' >&2; sleep 30",
        ]),
        std::time::Duration::from_millis(200),
    );
    assert!(child.timed_out);
    assert_eq!(process::return_code(child.status), Some(-9));
    assert_eq!(child.stdout, b"budget-partial");
    assert_eq!(child.stderr, b"error");
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
}
#[test]
fn deep_child() {
    if std::env::var_os("ASIDE_DEEP_CHILD").is_none() {
        return;
    }
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            for count in [5_000, 50_000] {
                let source = "if true {} else ".repeat(count) + "if true {}";
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
        })
        .unwrap()
        .join()
        .unwrap();
}
