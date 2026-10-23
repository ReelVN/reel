//! The author command explores the existing game's UI and narrative inputs.
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

struct Project(tempfile::TempDir);

impl Project {
    fn new() -> Self {
        let artifacts = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/reel-artifacts/cli-game-tests");
        fs::create_dir_all(&artifacts).unwrap();
        let directory = tempfile::tempdir_in(artifacts).unwrap();
        fs::create_dir(directory.path().join("src")).unwrap();
        fs::create_dir(directory.path().join("resources")).unwrap();
        Self(directory)
    }

    fn root(&self) -> &Path {
        self.0.path()
    }

    fn write(&self, path: &str, source: &str) {
        let path = self.root().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }

    fn command(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_reel"))
            .current_dir(self.root())
            .args(arguments)
            .output()
            .unwrap()
    }

    fn report(&self, arguments: &[&str]) -> (Output, Value) {
        let output = self.command(arguments);
        let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "invalid report: {error}; stdout={}; stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output, report)
    }
}

#[test]
fn editable_control_candidates_run_once_and_replay_checks_their_initial_values() {
    let project = Project::new();
    project.write(
        "src/main.aside",
        r#"
fn main() {
    game.configure(width:200,height:192);
    stage.replace(column(width:200,height:192) {
        slider(value:0.5,width:200,height:48,on_change:|event| {});
        slider(value:0.5,width:200,height:48,on_change:|event| {});
        toggle("Flag",width:200,height:48,on_change:|event| {});
        button("Exit",width:200,height:48,on_click:|| game.exit());
    });
}
"#,
    );
    let (_, report) = project.report(&["test", "--format", "json"]);
    assert_eq!(report["status"], "incomplete", "{report}");
    let runs = report["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 8, "{report}");
    let mut selected = Vec::new();
    for run in runs {
        let actions = run["source"]["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 1, "{run}");
        assert_eq!(
            actions[0]["option_keys"],
            runs[0]["source"]["actions"][0]["option_keys"]
        );
        assert_eq!(run["source"]["actions"], run["asb"]["actions"]);
        assert!(run["replay_mismatch"].is_null(), "{run}");
        selected.push(actions[0]["selected"].as_u64().unwrap());
    }
    selected.sort_unstable();
    assert_eq!(selected, (0..8).collect::<Vec<_>>());
    let changed = runs
        .iter()
        .find(|run| {
            run["source"]["diagnostic"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("editable control values changed"))
        })
        .expect("editing is recorded as an incomplete observable difference");
    let mut trace: Value = serde_json::from_slice(
        &fs::read(project.root().join(changed["replay"].as_str().unwrap())).unwrap(),
    )
    .unwrap();
    trace["boundary"] = Value::Null;
    trace["actions"][0]["selected"] = Value::from(7);
    project.write("exit.json", &serde_json::to_string(&trace).unwrap());
    let (_, replay) = project.report(&["test", "--replay", "exit.json", "--format", "json"]);
    assert_eq!(replay["status"], "passed", "{replay}");
    trace["actions"][0]["input_state"] = Value::String("00".repeat(32));
    project.write("wrong-state.json", &serde_json::to_string(&trace).unwrap());
    let (_, wrong) = project.report(&["test", "--replay", "wrong-state.json", "--format", "json"]);
    assert_eq!(wrong["status"], "failed", "{wrong}");
    for pass in ["source", "asb"] {
        assert_eq!(wrong["runs"][0][pass]["outcome"], "replay_mismatch");
    }
    let journal = fs::read_to_string(project.root().join("build/test/progress.jsonl")).unwrap();
    for line in journal.lines() {
        let event: Value = serde_json::from_str(line).unwrap();
        if event["event"] == "terminal" {
            assert!(event["actions"].is_number());
            assert!(event.get("report").is_none());
        }
    }
}

#[test]
fn an_observed_incomplete_menu_replays_at_its_exact_boundary() {
    let project = Project::new();
    project.write(
        "src/main.aside",
        r#"
fn main() {
    game.configure(width:160,height:100);
    stage.replace(column(width:160,height:100) {
        button("Stay",width:160,height:48,on_click:|| {});
        button("Exit",width:160,height:48,on_click:|| game.exit());
    });
}
"#,
    );
    let (_, report) = project.report(&["test", "--format", "json"]);
    assert_eq!(report["status"], "incomplete", "{report}");
    let cycle = report["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["source"]["outcome"] == "ui_cycle")
        .unwrap();
    let replay = cycle["replay"].as_str().unwrap();
    let (_, replayed) = project.report(&["test", "--replay", replay, "--format", "json"]);
    assert_eq!(replayed["status"], "incomplete", "{replayed}");
    for pass in ["source", "asb"] {
        assert_eq!(replayed["runs"][0][pass]["outcome"], "ui_cycle");
        assert_eq!(replayed["runs"][0][pass]["actions"], cycle[pass]["actions"]);
    }
    let mut trace: Value =
        serde_json::from_slice(&fs::read(project.root().join(replay)).unwrap()).unwrap();
    trace["boundary"]["fingerprint"] = Value::String("00".repeat(32));
    project.write(
        "changed-boundary.json",
        &serde_json::to_string(&trace).unwrap(),
    );
    let (_, changed) = project.report(&[
        "test",
        "--replay",
        "changed-boundary.json",
        "--format",
        "json",
    ]);
    assert_eq!(changed["status"], "failed");
    assert_eq!(changed["runs"][0]["source"]["outcome"], "replay_mismatch");
}

#[test]
fn an_early_fault_publishes_its_partial_diagnostic_and_replay_before_exploration_finishes() {
    let project = Project::new();
    project.write(
        "src/main.aside",
        r#"
fn main() {
    stage.replace(stack());
    let route = choose([{key:"fault",label:"Fault"},{key:"waiting",label:"Waiting"}]);
    if route == "fault" { throw "early source fault"; }
    loop { await delay(10).run(); }
}
"#,
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_reel"))
        .current_dir(project.root())
        .args(["test", "--format", "json"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let observed = loop {
        let partial = fs::read(project.root().join("build/test/partial.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        if let Some(partial) = partial
            && partial["failures"].as_u64().unwrap_or(0) != 0
            && partial["latest_progress"]["logical_ns"]
                .as_u64()
                .unwrap_or(0)
                > 10_000_000_000
        {
            break Some(partial);
        }
        if child.try_wait().unwrap().is_some() || started.elapsed() > Duration::from_secs(20) {
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let running = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    let partial = observed.unwrap_or_else(|| {
        panic!(
            "early failure was not published: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert!(running, "the sibling time loop must still be executing");
    assert_eq!(partial["complete"], false);
    assert_eq!(partial["status"], "failed");
    assert!(partial["latest_progress"]["steps"].as_u64().unwrap() > 0);
    assert_eq!(partial["latest"]["pass"], "source");
    assert!(
        partial["latest"]["diagnostic"]["message"]
            .as_str()
            .unwrap()
            .contains("early source fault")
    );
    assert!(!project.root().join("build/test/report.json").exists());
    let replay = partial["latest"]["replay"].as_str().unwrap();
    let trace: Value =
        serde_json::from_slice(&fs::read(project.root().join(replay)).unwrap()).unwrap();
    assert_eq!(trace["actions"][0]["selected"], 0);
    let progress = fs::read_to_string(project.root().join("build/test/progress.jsonl")).unwrap();
    let events = progress
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(
        events
            .iter()
            .filter(|event| event["event"] == "progress")
            .count()
            >= 2
    );
    assert!(
        events
            .iter()
            .any(|event| event["event"] == "terminal" && event["outcome"] == "runtime_failure")
    );
    assert!(
        !events
            .iter()
            .any(|event| event["event"] == "pass_started" && event["pass"] == "asb")
    );
}

#[test]
fn game_menu_and_title_lead_to_hidden_second_choice_fault_and_exact_replay() {
    let project = Project::new();
    project.write(
        "src/main.aside",
        r#"
use game::story::title;
fn main() {
    game.configure(width: 320, height: 240);
    stage.replace(button("Open", width: 160, height: 48, on_click: || game.start(title)));
}
"#,
    );
    project.write(
        "src/story.aside",
        r#"
pub fn title() {
    stage.replace(button("Begin", width: 160, height: 48, on_click: || game.start(play)));
}
fn play() {
    await run(delay(120));
    let model = game.choice();
    stage.replace(view(|| column(width: 240) {
        for option in model.options {
            button(option.label, width: 160, height: 48,
                on_click: || model.select(option.id));
        }
    }));
    let answer = choose([{key: "safe", label: "Continue"}, {key: "fault", label: "Inspect"}]);
    if answer == "fault" { throw "hidden second route fault"; }
    game.exit();
}
"#,
    );
    project.write("tests/unused.aside", "this is deliberately invalid source");
    let started = Instant::now();
    let (output, report) = project.report(&["test", "--format", "json"]);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(report["status"], "failed", "{report}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "a two-minute delay must use virtual time"
    );
    let runs = report["runs"].as_array().unwrap();
    assert!(
        runs.iter()
            .any(|run| run["source"]["outcome"] == "finished"),
        "{report}"
    );
    let failure = runs
        .iter()
        .find(|run| run["source"]["outcome"] == "runtime_failure")
        .unwrap_or_else(|| panic!("second choice was not explored: {report}"));
    let actions = failure["source"]["actions"].as_array().unwrap();
    assert!(
        actions
            .iter()
            .filter(|action| action["kind"] == "controls")
            .count()
            >= 2,
        "{report}"
    );
    assert!(
        actions
            .iter()
            .any(|action| action["kind"] == "choice" && action["selected"] == 1),
        "{report}"
    );
    for kind in ["source", "asb"] {
        assert_eq!(failure[kind]["outcome"], "runtime_failure");
        assert!(
            failure[kind]["diagnostic"]["message"]
                .as_str()
                .unwrap()
                .contains("hidden second route fault")
        );
        assert!(failure[kind]["diagnostic"]["stack"].as_array().unwrap().iter()
            .any(|frame| frame["path"] == "src/story.aside" && frame["line"].as_u64().is_some()));
    }
    let saved: Value =
        serde_json::from_slice(&fs::read(project.root().join("build/test/report.json")).unwrap())
            .unwrap();
    assert_eq!(saved, report);
    let trace = report["replays"]
        .as_array()
        .unwrap()
        .first()
        .unwrap()
        .as_str()
        .unwrap();
    let (output, replay) = project.report(&["test", "--replay", trace, "--format", "json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(replay["status"], "failed");
    assert_eq!(replay["runs"].as_array().unwrap().len(), 1);
    assert_eq!(
        replay["runs"][0]["source"]["actions"],
        failure["source"]["actions"]
    );
    let human = project.command(&["test", "--replay", trace]);
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("src/story.aside:"), "{human}");
    assert!(human.contains("hidden second route fault"), "{human}");
    for line in human
        .lines()
        .filter(|line| line.contains("hidden second route fault"))
    {
        assert_eq!(line.matches("src/story.aside:").count(), 1, "{line}");
    }
    project.write("src/main.aside", "fn main() { game.exit(); }");
    let (_, stale) = project.report(&["test", "--replay", trace, "--format", "json"]);
    assert_eq!(stale["stage"], "replay");
    assert!(
        stale["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("digest")
    );
}

#[test]
fn ordinary_game_exit_passes_and_source_or_resource_faults_remain_failures() {
    let project = Project::new();
    project.write("src/main.aside", "fn main() { game.exit(); }");
    let (output, report) = project.report(&["test", "--format", "json"]);
    assert!(output.status.success(), "{report}");
    assert_eq!(report["status"], "passed");
    assert_eq!(report["source"], "src/main.aside");
    project.write(
        "src/main.aside",
        "use game::broken::open; fn main(){open();}",
    );
    project.write("src/broken.aside", "pub fn open(){\nmissing_name;\n}");
    let (output, report) = project.report(&["test", "--format", "json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(report["diagnostics"][0]["source"], "src/broken.aside");
    assert_eq!(report["diagnostics"][0]["range"]["start"]["line"], 1);
    project.write(
        "src/main.aside",
        "fn main(){stage.replace(image(\"missing.png\"));}",
    );
    let (output, report) = project.report(&["test", "--format", "json"]);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(report["status"], "failed");
    assert!(report.to_string().contains("missing.png"), "{report}");
}

#[test]
fn replay_restores_the_selected_external_resource_directory() {
    let project = Project::new();
    project.write(
        "src/main.aside",
        "fn main(){stage.replace(image(\"picture.png\")); await stage.presented(); throw \"after external image\";}",
    );
    let external = project.root().join("external");
    fs::create_dir(&external).unwrap();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]))
        .save(external.join("picture.png"))
        .unwrap();
    let (_, original) = project.report(&["test", "--assets", "external", "--format", "json"]);
    assert_eq!(
        original["runs"][0]["source"]["outcome"], "runtime_failure",
        "{original}"
    );
    let trace = original["replays"][0].as_str().unwrap();
    let (_, replay) = project.report(&["test", "--replay", trace, "--format", "json"]);
    assert_eq!(
        replay["runs"][0]["source"]["outcome"], "runtime_failure",
        "{replay}"
    );
    assert!(
        replay["runs"][0]["source"]["diagnostic"]["message"]
            .as_str()
            .unwrap()
            .contains("after external image")
    );
}

#[test]
fn replay_preserves_implicit_random_streams_and_civil_time() {
    let project = Project::new();
    project.write(
        "src/main.aside",
        r#"
use std::time::now;
fn main() {
    stage.replace(stack());
    let value = random().int(0, 9000000000);
    let stamp = now();
    let key = f"{value}:{stamp}";
    let answer = choose([{key: "safe", label: "Continue"}, {key: key, label: key}]);
    if answer == key { throw key; }
    game.exit();
}
"#,
    );
    let (_, original) = project.report(&["test", "--format", "json"]);
    assert_eq!(original["status"], "failed", "{original}");
    let fault = original["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["source"]["outcome"] == "runtime_failure")
        .unwrap();
    let key = fault["source"]["actions"][0]["option_keys"][1]
        .as_str()
        .unwrap();
    assert_eq!(fault["source"]["actions"], fault["asb"]["actions"]);
    assert!(
        fault["asb"]["diagnostic"]["message"]
            .as_str()
            .unwrap()
            .contains(key)
    );
    let trace_path = original["replays"][0].as_str().unwrap();
    let (_, replay) = project.report(&["test", "--replay", trace_path, "--format", "json"]);
    assert_eq!(replay["environment"], original["environment"]);
    assert_eq!(
        replay["runs"][0]["source"]["actions"],
        fault["source"]["actions"]
    );
    let mut trace: Value =
        serde_json::from_slice(&fs::read(project.root().join(trace_path)).unwrap()).unwrap();
    trace["environment"]["unix_seconds"] = Value::from(0);
    let value = key.split(':').next().unwrap();
    let fixed_key = format!("{value}:0");
    trace["actions"][0]["option_keys"][1] = Value::from(fixed_key.clone());
    fs::write(
        project.root().join("fixed-time.json"),
        serde_json::to_vec(&trace).unwrap(),
    )
    .unwrap();
    let (_, fixed) = project.report(&["test", "--replay", "fixed-time.json", "--format", "json"]);
    assert_eq!(
        fixed["runs"][0]["source"]["outcome"], "runtime_failure",
        "{fixed}"
    );
    assert!(
        fixed["runs"][0]["source"]["diagnostic"]["message"]
            .as_str()
            .unwrap()
            .contains(&fixed_key)
    );
}

#[test]
fn generated_help_and_usage_keep_game_testing_options_together() {
    let project = Project::new();
    project.write("src/main.aside", "fn main(){game.exit();}");
    let output = project.command(&["test", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for option in ["--format", "--assets", "--replay"] {
        assert!(help.contains(option), "{help}");
    }
    for obsolete in ["--case", "--max-runs", "--max-steps", "--timeout"] {
        assert!(!help.contains(obsolete), "{help}");
        let (output, report) = project.report(&["test", obsolete, "1", "--format", "json"]);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(report["stage"], "usage");
    }
}
