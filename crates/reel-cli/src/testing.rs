//! Project compilation and artifact publication for automatic game exploration.
use crate::commands::Test;
use anyhow::Context;
use aside_compiler::{SourceMap, semantic::SourceModule};
use reel_dev::{
    headless::{self, RunEnvironment, RunOptions},
    testing::{self, ExplorationEvent, ExplorationPass, ReplayTrace, RunPair, TestStatus},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

struct ProgressJournal {
    writer: BufWriter<fs::File>,
    artifacts: PathBuf,
    project: PathBuf,
    source_digest: Option<String>,
    environment: Option<RunEnvironment>,
    assets: Option<PathBuf>,
    started: Instant,
    last_display: Option<Instant>,
    stage: String,
    decisions: u64,
    source_terminals: u64,
    asb_terminals: u64,
    failures: u64,
    incomplete: u64,
    latest: Value,
    latest_progress: Value,
    complete: bool,
    final_status: Option<String>,
}

impl ProgressJournal {
    fn new(project: &Path, artifacts: &Path) -> io::Result<Self> {
        Ok(Self {
            writer: BufWriter::new(fs::File::create(artifacts.join("progress.jsonl"))?),
            artifacts: artifacts.to_path_buf(),
            project: project.to_path_buf(),
            source_digest: None,
            environment: None,
            assets: None,
            started: Instant::now(),
            last_display: None,
            stage: "loading".into(),
            decisions: 0,
            source_terminals: 0,
            asb_terminals: 0,
            failures: 0,
            incomplete: 0,
            latest: Value::Null,
            latest_progress: Value::Null,
            complete: false,
            final_status: None,
        })
    }

    fn append(&mut self, value: &impl serde::Serialize) -> io::Result<()> {
        serde_json::to_writer(&mut self.writer, value)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()
    }

    fn stage(&mut self, stage: &str) -> io::Result<()> {
        self.stage = stage.into();
        self.append(&json!({"event":"stage", "stage":stage}))?;
        eprintln!("reel test: {stage}");
        self.write_partial()
    }

    fn write_partial(&self) -> io::Result<()> {
        let partial = json!({
            "schema_version": 1, "complete": self.complete,
            "status": self.final_status.as_deref().unwrap_or(if self.failures != 0 { "failed" } else { "incomplete" }),
            "project": self.project, "source_digest": self.source_digest,
            "environment": self.environment, "stage": self.stage,
            "elapsed_seconds": self.started.elapsed().as_secs_f64(),
            "decisions": self.decisions, "source_terminals": self.source_terminals,
            "asb_terminals": self.asb_terminals, "failures": self.failures,
            "incomplete_paths": self.incomplete, "latest": self.latest,
            "latest_progress": self.latest_progress,
            "progress": "build/test/progress.jsonl",
        });
        fs::write(
            self.artifacts.join("partial.json"),
            serde_json::to_vec_pretty(&partial)?,
        )
    }

    fn event(&mut self, event: &ExplorationEvent<'_>) -> anyhow::Result<()> {
        if let ExplorationEvent::Terminal {
            pass,
            requested_prefix,
            reached_prefix,
            report,
            boundary,
        } = event
        {
            self.append(&json!({"event":"terminal", "pass":pass,
                "requested_prefix":requested_prefix,"reached_prefix":reached_prefix,
                "outcome":report.outcome,"actions":report.actions.len(),
                "steps":report.steps,"diagnostic":report.diagnostic,
                "boundary":boundary}))?;
        } else {
            self.append(event)?;
        }
        let display = self
            .last_display
            .is_none_or(|last| last.elapsed() >= Duration::from_secs(1));
        match event {
            ExplorationEvent::PassStarted { pass } => {
                self.stage = pass_name(*pass).into();
                eprintln!("reel test: {} execution", self.stage);
                self.last_display = Some(Instant::now());
                self.write_partial()?;
            }
            ExplorationEvent::Progress {
                pass,
                steps,
                logical_ns,
                accepted_frames,
                interaction,
                interaction_generation,
                content_id,
                read_content,
            } => {
                self.latest_progress = json!({"pass":pass,"steps":steps,"logical_ns":logical_ns,
                    "accepted_frames":accepted_frames,"interaction":interaction,
                    "interaction_generation": interaction_generation,
                    "content_id": content_id, "read_content": read_content});
                if display {
                    eprintln!(
                        "reel test: {} steps={steps} virtual_seconds={:.3} frames={accepted_frames} read={read_content} interaction={} generation={interaction_generation:?}",
                        pass_name(*pass),
                        *logical_ns as f64 / 1_000_000_000.,
                        interaction.unwrap_or("none")
                    );
                    self.last_display = Some(Instant::now());
                }
                self.write_partial()?;
            }
            ExplorationEvent::Decision {
                pass,
                prefix,
                depth,
                kind: _,
                input_state: _,
                option_keys,
                observed_states,
                frontier,
            } => {
                self.decisions += 1;
                if display {
                    eprintln!(
                        "reel test: {} prefix={prefix} depth={depth} options={} observed={observed_states} checkpoints={frontier}",
                        pass_name(*pass),
                        option_keys.len()
                    );
                    self.last_display = Some(Instant::now());
                    self.write_partial()?;
                }
            }
            ExplorationEvent::Branch { .. } => {}
            ExplorationEvent::Terminal {
                pass,
                requested_prefix,
                reached_prefix,
                report,
                boundary,
            } => {
                match pass {
                    ExplorationPass::Source => self.source_terminals += 1,
                    ExplorationPass::Asb => self.asb_terminals += 1,
                }
                let failed = matches!(
                    report.outcome,
                    headless::RunOutcome::RuntimeFailure
                        | headless::RunOutcome::ResourceFailure
                        | headless::RunOutcome::ReplayMismatch
                );
                if failed {
                    self.failures += 1;
                }
                if !failed && report.outcome != headless::RunOutcome::Finished {
                    self.incomplete += 1;
                }
                let replay = if failed {
                    let number = self.source_terminals + self.asb_terminals;
                    let name = format!("replay-partial-{}-{number:06}.json", pass_name(*pass));
                    let trace = ReplayTrace {
                        schema_version: testing::SCHEMA_VERSION,
                        source_digest: self
                            .source_digest
                            .clone()
                            .context("progress source digest")?,
                        environment: self
                            .environment
                            .clone()
                            .context("progress host environment")?,
                        assets: self.assets.clone(),
                        actions: report.actions.iter().cloned().collect(),
                        boundary: boundary.cloned(),
                    };
                    fs::write(
                        self.artifacts.join(&name),
                        serde_json::to_vec_pretty(&trace)?,
                    )?;
                    Some(format!("build/test/{name}"))
                } else {
                    None
                };
                self.latest = json!({"pass":pass, "requested_prefix":requested_prefix,
                    "reached_prefix":reached_prefix, "outcome":report.outcome,
                    "actions":report.actions.len(), "diagnostic":report.diagnostic, "replay":replay});
                if failed || display {
                    eprintln!(
                        "reel test: {} {:?} at prefix {reached_prefix} ({} actions)",
                        pass_name(*pass),
                        report.outcome,
                        report.actions.len()
                    );
                    if failed && let Some(diagnostic) = &report.diagnostic {
                        eprintln!("  {}", diagnostic.message);
                        if let Some(path) = replay {
                            eprintln!("  Replay: reel test --replay \"{path}\"");
                        }
                    }
                    self.last_display = Some(Instant::now());
                }
                self.write_partial()?;
            }
        }
        Ok(())
    }

    fn finish(&mut self, report: &Value) -> io::Result<()> {
        self.complete = true;
        self.final_status = report["status"].as_str().map(str::to_owned);
        self.stage = "complete".into();
        if report["status"] == "failed" && self.failures == 0 {
            self.failures = 1;
        }
        if let Some(diagnostics) = report.get("diagnostics") {
            self.latest = diagnostics.clone();
        }
        self.append(&json!({"event":"complete", "status":report["status"]}))?;
        self.write_partial()
    }
}

fn pass_name(pass: ExplorationPass) -> &'static str {
    match pass {
        ExplorationPass::Source => "source",
        ExplorationPass::Asb => "asb",
    }
}

fn run_trace(
    story: reel_engine::aside::source::CompiledStory,
    assets: &Path,
    options: &RunOptions,
    trace: &ReplayTrace,
) -> anyhow::Result<headless::RunReport> {
    if let Some(boundary) = &trace.boundary {
        headless::run_to_boundary(story, assets, options, &trace.actions, boundary)
    } else {
        headless::run(story, assets, options, &trace.actions)
    }
}

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
    progress: &mut ProgressJournal,
) -> io::Result<Value> {
    progress.stage("loading")?;
    let loaded = match reel_cli::project::load(root) {
        Ok(loaded) => loaded,
        Err(error) => return Ok(load_failure(error)),
    };
    progress.stage("checking")?;
    let checked = match loaded.check() {
        Ok(checked) => checked,
        Err(error) => return Ok(compilation_failure(&loaded.sources, error)),
    };
    progress.stage("localization")?;
    if let Err(error) = reel_cli::localization::check_project(root, &checked.catalogs) {
        return Ok(setup_failure("localization", error.to_string()));
    }
    progress.stage("building")?;
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
    progress.source_digest = Some(source_digest.clone());
    progress.environment = Some(environment.clone());
    progress.assets = recorded_assets.clone();
    drop(loaded);
    let exploration = if let Some(replay) = replay {
        let options = RunOptions {
            strict_replay: true,
            environment: environment.clone(),
        };
        progress
            .event(&ExplorationEvent::PassStarted {
                pass: ExplorationPass::Source,
            })
            .map_err(io::Error::other)?;
        run_trace(source, &assets, &options, replay)
            .and_then(|source| {
                progress.event(&ExplorationEvent::Terminal {
                    pass: ExplorationPass::Source,
                    requested_prefix: 0,
                    reached_prefix: 0,
                    report: &source,
                    boundary: replay
                        .boundary
                        .as_ref()
                        .filter(|boundary| boundary.outcome == source.outcome),
                })?;
                progress.event(&ExplorationEvent::PassStarted {
                    pass: ExplorationPass::Asb,
                })?;
                run_trace(asb, &assets, &options, replay).and_then(|asb| {
                    progress.event(&ExplorationEvent::Terminal {
                        pass: ExplorationPass::Asb,
                        requested_prefix: 0,
                        reached_prefix: 0,
                        report: &asb,
                        boundary: replay
                            .boundary
                            .as_ref()
                            .filter(|boundary| boundary.outcome == asb.outcome),
                    })?;
                    Ok(RunPair { source, asb })
                })
            })
            .map(|pair| {
                let mut report = testing::replay(&replay.actions, pair);
                report.runs[0].boundary = replay
                    .boundary
                    .clone()
                    .filter(|boundary| boundary.outcome == report.runs[0].source.outcome);
                report
            })
    } else {
        headless::explore_with_observer(source, asb, &assets, &environment, |event| {
            progress.event(event)
        })
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
        if run.source.outcome == headless::RunOutcome::CheckpointUnavailable
            && run.boundary.is_none()
        {
            continue;
        }
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
            actions: run.source.actions.iter().cloned().collect(),
            boundary: run.boundary.clone(),
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
    let mut progress = ProgressJournal::new(root, artifacts)?;
    let mut report = execute(root, assets, replay, artifacts, &mut progress)?;
    report["schema_version"] = json!(1);
    report["source"] = json!("src/main.aside");
    report["scope"] = json!("headless-source-asb-input-paths");
    report["project"] = json!(root);
    report["report"] = json!("build/test/report.json");
    fs::write(
        artifacts.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    progress.finish(&report)?;
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
