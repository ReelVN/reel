//! Project compilation and artifact publication for automatic game exploration.
use crate::commands::Test;
use aside_compiler::{SourceMap, semantic::SourceModule};
use reel_dev::{
    headless::{self, RunEnvironment, RunOptions},
    testing::{self, ReplayTrace, RunPair, TestStatus},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

fn compilation_failure(sources: &[SourceModule], failure: reel_cli::Failure) -> Value {
    let diagnostics: Vec<_> = failure
        .diagnostics
        .iter()
        .map(|reported| {
            let source = sources
                .iter()
                .find(|source| reported.source.as_deref() == source.source_path.as_deref())
                .or_else(|| sources.first());
            let mut diagnostic = reel_cli::diagnostic_json(
                &reported.diagnostic,
                &SourceMap::new(source.map_or("", |source| &source.source)),
            );
            diagnostic["source"] = json!(reported.source.as_deref().unwrap_or("src/main.aside"));
            if let Some(code) = &failure.code {
                diagnostic["code"] = json!(code);
            }
            diagnostic
        })
        .collect();
    json!({"status":"failed","stage":failure.stage,"diagnostics":diagnostics,"runs":[],"replays":[]})
}

fn setup_failure(stage: &str, message: String) -> Value {
    json!({"status":"failed","stage":stage,
        "diagnostics":[{"code":"REEL-TEST-SETUP","source":"src/main.aside","message":message,"range":null}],
        "runs":[],"replays":[]})
}

fn load_failure(error: reel_cli::project::LoadError) -> Value {
    if let Some(input) = error.input_error {
        return json!({"status":"failed","stage":"input",
            "diagnostics":[{"code":input.code,"source":"src/main.aside","message":input.message,
                "range":input.byte_range.map(|range| json!({"byte_start":range.start,"byte_end":range.end,"start":null,"end":null}))}],
            "runs":[],"replays":[]});
    }
    compilation_failure(&error.sources, error.failure)
}

fn execute(
    root: &Path,
    assets: Option<&Path>,
    replay: Option<&ReplayTrace>,
    artifacts: &Path,
) -> io::Result<Value> {
    let loaded = match reel_cli::project::load(root) {
        Ok(loaded) => loaded,
        Err(error) => return Ok(load_failure(error)),
    };
    let checked = match loaded.check() {
        Ok(checked) => checked,
        Err(error) => return Ok(compilation_failure(&loaded.sources, error)),
    };
    if let Err(error) = reel_cli::localization::check_project(root, &checked.catalogs) {
        return Ok(setup_failure("localization", error.to_string()));
    }
    let artifact = match reel_cli::build(&checked, Some(reel_cli::project::ENTRY)) {
        Ok(artifact) => artifact,
        Err(error) => return Ok(compilation_failure(&loaded.sources, error)),
    };
    if let Some(replay) = replay
        && replay.source_digest != artifact.blake3
    {
        return Ok(setup_failure(
            "replay",
            "replay source digest differs from the current game and imported modules".into(),
        ));
    }
    let asb = match reel_cli::decode_story(&artifact.bytes) {
        Ok(story) => story,
        Err(error) => return Ok(setup_failure("package", error.to_string())),
    };
    let source_digest = artifact.blake3.clone();
    drop(artifact);
    let source = match reel_cli::into_story(checked, reel_cli::project::ENTRY) {
        Ok(story) => story,
        Err(error) => return Ok(compilation_failure(&loaded.sources, error)),
    };
    let recorded_assets = assets.map(Path::to_path_buf);
    let assets = recorded_assets.clone().unwrap_or_else(|| loaded.assets());
    let environment =
        replay.map_or_else(RunEnvironment::capture, |trace| trace.environment.clone());
    drop(loaded);
    let exploration = if let Some(replay) = replay {
        let options = RunOptions {
            strict_replay: true,
            environment: environment.clone(),
        };
        headless::run(source, &assets, &options, &replay.actions)
            .and_then(|source| {
                headless::run(asb, &assets, &options, &source.actions)
                    .map(|asb| RunPair { source, asb })
            })
            .map(|pair| testing::replay(&replay.actions, pair))
    } else {
        headless::explore_with_environment(source, asb, &assets, &environment)
    };
    let exploration = match exploration {
        Ok(report) => report,
        Err(error) => return Ok(setup_failure("runtime", error.to_string())),
    };
    let mut report = serde_json::to_value(&exploration)?;
    report["source_digest"] = json!(source_digest);
    report["environment"] = json!(environment);
    report["replays"] = json!([]);
    for (index, run) in exploration.runs.iter().enumerate() {
        if run.source.outcome == headless::RunOutcome::Finished
            && run.asb.outcome == headless::RunOutcome::Finished
            && run.replay_mismatch.is_none()
        {
            continue;
        }
        let trace = ReplayTrace {
            schema_version: 1,
            source_digest: source_digest.clone(),
            environment: environment.clone(),
            assets: recorded_assets.clone(),
            actions: run.source.actions.clone(),
        };
        let replay_number = report["replays"].as_array().unwrap().len() + 1;
        let relative = format!("build/test/replay-{replay_number:03}.json");
        let destination = artifacts.join(format!("replay-{replay_number:03}.json"));
        fs::write(destination, serde_json::to_vec_pretty(&trace)?)?;
        report["runs"][index]["replay"] = json!(relative);
        report["replays"]
            .as_array_mut()
            .unwrap()
            .push(json!(relative));
    }
    Ok(report)
}

fn human_report(report: &Value) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(
        output,
        "src/main.aside: {} ({} input paths)",
        report["status"].as_str().unwrap(),
        report["runs"].as_array().map_or(0, Vec::len)
    )?;
    if let Some(diagnostics) = report["diagnostics"].as_array() {
        for diagnostic in diagnostics {
            let source = diagnostic["source"].as_str().unwrap_or("reel");
            let message = diagnostic["message"].as_str().unwrap_or("test failed");
            if let Some(line) = diagnostic["range"]["start"]["line"].as_u64() {
                writeln!(output, "  {source}:{}: {message}", line + 1)?;
            } else {
                writeln!(output, "  {source}: {message}")?;
            }
        }
    }
    for run in report["runs"].as_array().into_iter().flatten() {
        for kind in ["source", "asb"] {
            let diagnostic = &run[kind]["diagnostic"];
            if let Some(message) = diagnostic["message"].as_str() {
                if let Some(frame) = diagnostic["stack"]
                    .as_array()
                    .and_then(|stack| stack.first())
                {
                    let location = format!(
                        "{}:{}:{}",
                        frame["path"].as_str().unwrap_or("<source>"),
                        frame["line"],
                        frame["column"]
                    );
                    if message.starts_with(&format!("{location}: ")) {
                        writeln!(output, "  {kind}: {message}")?;
                    } else {
                        writeln!(output, "  {kind}: {location}: {message}")?;
                    }
                } else {
                    writeln!(output, "  {kind}: {message}")?;
                }
            }
        }
        if let Some(message) = run["replay_mismatch"].as_str() {
            writeln!(output, "  {message}")?;
        }
    }
    for replay in report["replays"].as_array().into_iter().flatten() {
        let path = Path::new(report["project"].as_str().unwrap()).join(replay.as_str().unwrap());
        writeln!(
            output,
            "  Replay: reel test --replay \"{}\"",
            path.display()
        )?;
    }
    writeln!(
        output,
        "Report: {}",
        Path::new(report["project"].as_str().unwrap())
            .join("build/test/report.json")
            .display()
    )
}

fn publish_report(
    root: &Path,
    assets: Option<&Path>,
    replay: Option<&ReplayTrace>,
    artifacts: &Path,
) -> io::Result<Value> {
    fs::create_dir_all(artifacts)?;
    let mut report = execute(root, assets, replay, artifacts)?;
    report["schema_version"] = json!(1);
    report["source"] = json!("src/main.aside");
    report["scope"] = json!("headless-source-asb-input-paths");
    report["project"] = json!(root);
    report["report"] = json!("build/test/report.json");
    fs::write(
        artifacts.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(report)
}

pub(super) fn run(args: Test) -> io::Result<u8> {
    let json_format = args.format.json();
    let project = match reel_cli::project::locate(Path::new(".")) {
        Ok(project) => project,
        Err(error) => {
            super::failure(json_format, "project", None, &error.to_string())?;
            return Ok(2);
        }
    };
    let replay = match args
        .replay
        .as_ref()
        .map(|path| {
            fs::read(path).and_then(|bytes| {
                serde_json::from_slice::<ReplayTrace>(&bytes).map_err(io::Error::other)
            })
        })
        .transpose()
    {
        Ok(replay) => replay,
        Err(error) => {
            super::failure(
                json_format,
                "replay",
                args.replay.as_deref().and_then(Path::to_str),
                &error.to_string(),
            )?;
            return Ok(2);
        }
    };
    if let Some(replay) = &replay
        && let Err(error) = replay.validate()
    {
        super::failure(json_format, "replay", None, &error.to_string())?;
        return Ok(2);
    }
    let assets = args
        .assets
        .or_else(|| replay.as_ref().and_then(|trace| trace.assets.clone()));
    let assets = match assets.as_ref().map(std::path::absolute).transpose() {
        Ok(assets) => assets,
        Err(error) => {
            super::failure(
                json_format,
                "assets",
                assets.as_deref().and_then(Path::to_str),
                &error.to_string(),
            )?;
            return Ok(2);
        }
    };
    let artifacts = project.root.join("build/test");
    let report = match publish_report(
        &project.root,
        assets.as_deref(),
        replay.as_ref(),
        &artifacts,
    ) {
        Ok(report) => report,
        Err(error) => {
            super::failure(
                json_format,
                "output",
                artifacts.to_str(),
                &error.to_string(),
            )?;
            return Ok(2);
        }
    };
    if json_format {
        super::report_json(&report)?;
    } else {
        human_report(&report)?;
    }
    Ok(u8::from(report["status"] != json!(TestStatus::Passed)))
}
