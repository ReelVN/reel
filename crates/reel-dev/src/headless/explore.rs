//! Short-prefix exploration using temporary production session checkpoints.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::Path,
    sync::Arc,
};

use anyhow::{Context, Result};
use reel_engine::aside::source::CompiledStory;
use reel_player::story_runtime::RuntimeCheckpoint;
use sha2::{Digest, Sha256};

use super::{
    ActionKind, ActionSelection, ActionTrace, Decision, DriveFailure, RunDiagnostic,
    RunEnvironment, RunOutcome, RunReport, Runner,
};
use crate::testing::{
    ExplorationEvent, ExplorationPass, ExplorationRecorder, ExplorationReport, ReplayBoundary,
    RunPair,
};

type Observer<'a> = dyn FnMut(&ExplorationEvent<'_>) -> Result<()> + 'a;

struct Branch {
    directory: tempfile::TempDir,
    actions: ActionTrace,
    choices: Vec<(usize, usize)>,
    next: usize,
    kind: ActionKind,
    keys: Arc<[String]>,
    input_state: Option<Arc<str>>,
    steps: u64,
    window: super::window::VirtualWindow,
    focus: reel_player::session::InputFocus,
}

#[derive(Default)]
struct Frontier {
    branches: BTreeMap<(usize, usize), Branch>,
    sequence: usize,
}

impl Frontier {
    fn len(&self) -> usize {
        self.branches.len()
    }

    fn precedes(&self, depth: usize) -> bool {
        self.branches
            .first_key_value()
            .is_some_and(|((pending, _), _)| *pending <= depth)
    }

    fn push(&mut self, branch: Branch) {
        let key = (branch.actions.len(), self.sequence);
        self.sequence += 1;
        self.branches.insert(key, branch);
    }
}

struct SourceRoute {
    requested: usize,
    report: RunReport,
    boundary: Option<ReplayBoundary>,
}

struct ObservedBoundary {
    fingerprint: String,
    first_prefix: usize,
    option_keys: Arc<[String]>,
    storage_change: Option<(String, String)>,
    input_changed: bool,
}

struct Observation {
    first_prefix: usize,
    storage: String,
    input_state: Option<Arc<str>>,
}

struct Fingerprint {
    visible: String,
    storage: String,
    exact: String,
}

#[derive(Default)]
struct PlanNode {
    prefix: usize,
    expected: Option<ActionSelection>,
    children: Vec<(usize, usize)>,
    terminals: Vec<usize>,
}

/// Explores the current game's accepted UI controls and narrative choices.
///
/// Source and ASB run in successive passes, keeping one live runtime at a time.
/// Source discovers routes through production session checkpoints; ASB strictly
/// follows the resulting action-prefix tree using its own checkpoints. Frontier
/// entries retain temporary files rather than live VM copies. Repeated observed
/// UI states remain incomplete with their exact action path and diagnostic.
pub fn explore(
    source: CompiledStory,
    asb: CompiledStory,
    resources: &Path,
) -> Result<ExplorationReport> {
    explore_with_environment(source, asb, resources, &RunEnvironment::capture())
}

/// Explores with the same root entropy and civil time in both isolated passes.
pub fn explore_with_environment(
    source: CompiledStory,
    asb: CompiledStory,
    resources: &Path,
    environment: &RunEnvironment,
) -> Result<ExplorationReport> {
    explore_with_observer(source, asb, resources, environment, |_| Ok(()))
}

/// Explores accepted input paths while publishing incremental pass and results.
///
/// Observations share control-state fingerprints across source branches. Reused
/// observations remain incomplete because hidden business-state equivalence is
/// not established. Observer failures stop execution with the original error.
pub fn explore_with_observer(
    source: CompiledStory,
    asb: CompiledStory,
    resources: &Path,
    environment: &RunEnvironment,
    mut observer: impl FnMut(&ExplorationEvent<'_>) -> Result<()>,
) -> Result<ExplorationReport> {
    let mut recorder = ExplorationRecorder::new();
    observer(&ExplorationEvent::PassStarted {
        pass: ExplorationPass::Source,
    })?;
    let source_routes = source_pass(source, resources, &mut recorder, environment, &mut observer)?;
    // source_pass cancels and drops all live state before constructing ASB.
    observer(&ExplorationEvent::PassStarted {
        pass: ExplorationPass::Asb,
    })?;
    let asb_reports = match asb_pass(
        asb,
        resources,
        &source_routes,
        &mut recorder,
        environment,
        &mut observer,
    ) {
        Ok(reports) => reports,
        Err(error) => {
            let mut reports = Vec::with_capacity(source_routes.len());
            for source in &source_routes {
                let report = RunReport {
                    outcome: RunOutcome::ReplayMismatch,
                    actions: ActionTrace::default(),
                    steps: 0,
                    elapsed: std::time::Duration::ZERO,
                    diagnostic: Some(RunDiagnostic {
                        message: format!("ASB exploration could not continue: {error:#}"),
                        stack: Vec::new(),
                    }),
                    accepted_frames: 0,
                    audio_frames: 0,
                    audio_peak: 0.,
                };
                observer(&ExplorationEvent::Terminal {
                    pass: ExplorationPass::Asb,
                    requested_prefix: source.requested,
                    reached_prefix: 0,
                    report: &report,
                    boundary: None,
                })?;
                reports.push(report);
            }
            reports
        }
    };
    for (source, asb) in source_routes.into_iter().zip(asb_reports) {
        recorder.record_boundary(
            source.requested,
            RunPair {
                source: source.report,
                asb,
            },
            source.boundary,
        )?;
    }
    Ok(recorder.finish())
}

/// Strictly replays a trace up to its exact recorded incomplete input boundary.
///
/// Reaching a different control set, storage projection or host-window state is
/// a replay mismatch. Matching the observation preserves its incomplete result.
pub fn run_to_boundary(
    story: CompiledStory,
    resources: &Path,
    options: &super::RunOptions,
    prefix: &[ActionSelection],
    boundary: &ReplayBoundary,
) -> Result<RunReport> {
    let mut runner = Runner::new_with_environment(story, resources, &options.environment)?;
    let mut report = loop {
        let decision = match runner.drive() {
            Ok(Some(decision)) => decision,
            Ok(None) => {
                break mismatch(
                    &runner,
                    "story ended before the recorded incomplete boundary",
                );
            }
            Err((outcome, diagnostic)) => break runner.report(outcome, Some(diagnostic)),
        };
        if runner.actions.len() == prefix.len() {
            break if boundary_matches(&runner, &decision, boundary)? {
                observed_report(&runner, boundary)
            } else {
                mismatch(
                    &runner,
                    "replay incomplete input boundary differs from the recorded observation",
                )
            };
        }
        let expected = &prefix[runner.actions.len()];
        if expected.kind != decision.kind
            || expected.option_keys != decision.option_keys
            || expected.input_state != decision.input_state
            || expected.selected >= decision.actions.len()
        {
            break mismatch(
                &runner,
                "input target identities differ from the recorded incomplete route",
            );
        }
        if let Err((outcome, diagnostic)) = runner.apply(&decision, expected.selected) {
            break runner.report(outcome, Some(diagnostic));
        }
    };
    if let Err(error) = runner.close() {
        append_cleanup(&mut report, error);
    }
    Ok(report)
}

fn input_boundary(
    runner: &Runner,
    decision: &Decision,
    report: &RunReport,
) -> Result<ReplayBoundary> {
    Ok(ReplayBoundary {
        outcome: report.outcome,
        kind: decision.kind,
        option_keys: decision.option_keys.clone(),
        fingerprint: control_fingerprint(runner, decision)?,
        reason: report
            .diagnostic
            .as_ref()
            .map_or_else(String::new, |diagnostic| diagnostic.message.clone()),
    })
}

fn boundary_matches(
    runner: &Runner,
    decision: &Decision,
    boundary: &ReplayBoundary,
) -> Result<bool> {
    Ok(decision.kind == boundary.kind
        && decision.option_keys == boundary.option_keys
        && control_fingerprint(runner, decision)? == boundary.fingerprint)
}

fn observed_report(runner: &Runner, boundary: &ReplayBoundary) -> RunReport {
    runner.report(
        boundary.outcome,
        Some(RunDiagnostic {
            message: boundary.reason.clone(),
            stack: Vec::new(),
        }),
    )
}

fn source_pass(
    story: CompiledStory,
    resources: &Path,
    recorder: &mut ExplorationRecorder,
    environment: &RunEnvironment,
    observer: &mut Observer<'_>,
) -> Result<Vec<SourceRoute>> {
    let mut runner = Runner::new_with_environment(story, resources, environment)?;
    let mut routes = Vec::new();
    let mut frontier = Frontier::default();
    let mut observed = BTreeMap::new();
    let mut current = 0;
    let mut requested = 0;
    loop {
        let result = drive_pass(&mut runner, ExplorationPass::Source, observer)?;
        let (report, repeated) = match result {
            Ok(Some(decision)) => {
                emit_decision(
                    observer,
                    ExplorationPass::Source,
                    &runner,
                    current,
                    &decision,
                    observed.len(),
                    frontier.len(),
                )?;
                if let Some(repeated) =
                    observe_boundary(&runner, &decision, current, &mut observed)?
                {
                    let report = cycle_report(&runner, &repeated);
                    let boundary = ReplayBoundary {
                        outcome: report.outcome,
                        kind: decision.kind,
                        option_keys: repeated.option_keys,
                        fingerprint: repeated.fingerprint,
                        reason: report.diagnostic.as_ref().unwrap().message.clone(),
                    };
                    (report, Some(boundary))
                } else {
                    anyhow::ensure!(
                        !decision.option_keys.is_empty(),
                        "input decision has no targets"
                    );
                    let mut choices = Vec::with_capacity(decision.option_keys.len());
                    for selected in 0..decision.option_keys.len() {
                        let id = recorder.branch(current, selection(&decision, selected))?;
                        observer(&ExplorationEvent::Branch {
                            parent_prefix: current,
                            prefix: id,
                            selected,
                        })?;
                        choices.push((selected, id));
                    }
                    let yielding = frontier.precedes(runner.actions.len());
                    let checkpoint = if choices.len() > 1 || yielding {
                        capture_branch(&mut runner, choices.clone(), &decision).map(|mut branch| {
                            branch.next = usize::from(!yielding);
                            frontier.push(branch);
                        })
                    } else {
                        Ok(())
                    };
                    if let Err(error) = checkpoint {
                        let report = checkpoint_failure(&runner, "capture", &error);
                        let boundary = input_boundary(&runner, &decision, &report)?;
                        (report, Some(boundary))
                    } else if yielding {
                        let (destination, result) = resume(&mut runner, &mut frontier)?
                            .context("queued source boundary disappeared")?;
                        current = destination;
                        requested = destination;
                        match result {
                            Ok(()) => continue,
                            Err((outcome, diagnostic)) => {
                                (runner.report(outcome, Some(diagnostic)), None)
                            }
                        }
                    } else {
                        current = choices[0].1;
                        match runner.apply(&decision, choices[0].0) {
                            Ok(()) => continue,
                            Err((outcome, diagnostic)) => {
                                (runner.report(outcome, Some(diagnostic)), None)
                            }
                        }
                    }
                }
            }
            terminal => (terminal_report(&runner, terminal), None),
        };
        record_source_terminal(&mut routes, requested, current, report, repeated, observer)?;
        loop {
            let Some((destination, result)) = resume(&mut runner, &mut frontier)? else {
                if let Err(error) = runner.close() {
                    append_cleanup(
                        &mut routes.last_mut().expect("completed source route").report,
                        error,
                    );
                }
                return Ok(routes);
            };
            current = destination;
            requested = destination;
            if let Err((outcome, diagnostic)) = result {
                record_source_terminal(
                    &mut routes,
                    requested,
                    current,
                    runner.report(outcome, Some(diagnostic)),
                    None,
                    observer,
                )?;
            } else {
                break;
            }
        }
    }
}

fn emit_decision(
    observer: &mut Observer<'_>,
    pass: ExplorationPass,
    runner: &Runner,
    prefix: usize,
    decision: &Decision,
    observed_states: usize,
    frontier: usize,
) -> Result<()> {
    observer(&ExplorationEvent::Decision {
        pass,
        prefix,
        depth: runner.actions.len(),
        kind: decision.kind,
        input_state: decision.input_state.as_deref(),
        option_keys: &decision.option_keys,
        observed_states,
        frontier,
    })
}

fn drive_pass(
    runner: &mut Runner,
    pass: ExplorationPass,
    observer: &mut Observer<'_>,
) -> Result<Result<Option<Decision>, DriveFailure>> {
    let mut failure = None;
    let result = runner.drive_observed(|progress| {
        observer(&ExplorationEvent::Progress {
            pass,
            steps: progress.steps,
            logical_ns: progress.logical_ns,
            accepted_frames: progress.accepted_frames,
            interaction: progress.interaction,
            interaction_generation: progress.interaction_generation,
            content_id: progress.content_id.as_deref(),
            read_content: progress.read_content,
        })
        .map_err(|error| {
            let diagnostic = RunDiagnostic {
                message: error.to_string(),
                stack: Vec::new(),
            };
            failure = Some(error);
            (RunOutcome::RuntimeFailure, diagnostic)
        })
    });
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(result)
}

fn record_source_terminal(
    routes: &mut Vec<SourceRoute>,
    requested: usize,
    reached: usize,
    report: RunReport,
    boundary: Option<ReplayBoundary>,
    observer: &mut Observer<'_>,
) -> Result<()> {
    observer(&ExplorationEvent::Terminal {
        pass: ExplorationPass::Source,
        requested_prefix: requested,
        reached_prefix: reached,
        report: &report,
        boundary: boundary.as_ref(),
    })?;
    routes.push(SourceRoute {
        requested,
        report,
        boundary,
    });
    Ok(())
}

fn asb_pass(
    story: CompiledStory,
    resources: &Path,
    sources: &[SourceRoute],
    recorder: &mut ExplorationRecorder,
    environment: &RunEnvironment,
    observer: &mut Observer<'_>,
) -> Result<Vec<RunReport>> {
    let plan = replay_plan(sources, recorder)?;
    let mut reports = vec![None; sources.len()];
    let mut runner = Runner::new_with_environment(story, resources, environment)?;
    let mut frontier = Frontier::default();
    let mut current = 0;
    loop {
        let result = drive_pass(&mut runner, ExplorationPass::Asb, observer)?;
        let node = &plan[current];
        match result {
            Ok(Some(decision)) => {
                emit_decision(
                    observer,
                    ExplorationPass::Asb,
                    &runner,
                    node.prefix,
                    &decision,
                    0,
                    frontier.len(),
                )?;
                for index in &node.terminals {
                    if node.expected.is_some()
                        && sources[*index].report.outcome == RunOutcome::CheckpointUnavailable
                    {
                        let report = runner.report(
                            RunOutcome::CheckpointUnavailable,
                            Some(RunDiagnostic {
                                message:
                                    "ASB reached a boundary whose source checkpoint was unavailable"
                                        .into(),
                                stack: Vec::new(),
                            }),
                        );
                        observer(&ExplorationEvent::Terminal {
                            pass: ExplorationPass::Asb,
                            requested_prefix: sources[*index].requested,
                            reached_prefix: node.prefix,
                            report: &report,
                            boundary: sources[*index].boundary.as_ref(),
                        })?;
                        reports[*index] = Some(report);
                    }
                }
                if node.expected.is_none() {
                    let report = if let Some(boundary) = node
                        .terminals
                        .iter()
                        .find_map(|index| sources[*index].boundary.as_ref())
                    {
                        if boundary_matches(&runner, &decision, boundary)? {
                            observed_report(&runner, boundary)
                        } else {
                            mismatch(
                                &runner,
                                "ASB incomplete input boundary differs from the recorded source observation",
                            )
                        }
                    } else if node.terminals.iter().any(|index| {
                        sources[*index].report.outcome == RunOutcome::CheckpointUnavailable
                    }) {
                        runner.report(
                            RunOutcome::CheckpointUnavailable,
                            Some(RunDiagnostic {
                                message:
                                    "ASB reached a boundary whose source checkpoint was unavailable"
                                        .into(),
                                stack: Vec::new(),
                            }),
                        )
                    } else {
                        mismatch(
                            &runner,
                            "ASB reached an additional input boundary after the recorded route",
                        )
                    };
                    assign(&plan, current, &mut reports, report, sources, observer)?;
                } else if node.expected.as_ref().is_some_and(|expected| {
                    expected.kind != decision.kind
                        || expected.option_keys != decision.option_keys
                        || expected.input_state != decision.input_state
                }) {
                    assign(
                        &plan,
                        current,
                        &mut reports,
                        mismatch(
                            &runner,
                            "ASB input target identities differ from the recorded source route",
                        ),
                        sources,
                        observer,
                    )?;
                } else {
                    anyhow::ensure!(
                        !node.children.is_empty(),
                        "replay plan has an input without a successor"
                    );
                    let checkpoint = if node.children.len() > 1 {
                        capture_branch(&mut runner, node.children.clone(), &decision)
                            .map(|branch| frontier.push(branch))
                    } else {
                        Ok(())
                    };
                    if let Err(error) = checkpoint {
                        assign(
                            &plan,
                            current,
                            &mut reports,
                            checkpoint_failure(&runner, "capture", &error),
                            sources,
                            observer,
                        )?;
                    } else {
                        let (selected, destination) = node.children[0];
                        current = destination;
                        match runner.apply(&decision, selected) {
                            Ok(()) => continue,
                            Err((outcome, diagnostic)) => {
                                assign(
                                    &plan,
                                    current,
                                    &mut reports,
                                    runner.report(outcome, Some(diagnostic)),
                                    sources,
                                    observer,
                                )?;
                            }
                        }
                    }
                }
            }
            terminal => assign(
                &plan,
                current,
                &mut reports,
                terminal_report(&runner, terminal),
                sources,
                observer,
            )?,
        }
        loop {
            let Some((destination, result)) = resume(&mut runner, &mut frontier)? else {
                if let Err(error) = runner.close()
                    && let Some(report) = reports.iter_mut().rev().flatten().next()
                {
                    append_cleanup(report, error);
                }
                for (index, report) in reports.iter_mut().enumerate() {
                    if report.is_none() {
                        let missing = mismatch(&runner, "ASB replay route was not executed");
                        observer(&ExplorationEvent::Terminal {
                            pass: ExplorationPass::Asb,
                            requested_prefix: sources[index].requested,
                            reached_prefix: plan[current].prefix,
                            report: &missing,
                            boundary: None,
                        })?;
                        *report = Some(missing);
                    }
                }
                return Ok(reports.into_iter().map(Option::unwrap).collect());
            };
            current = destination;
            if let Err((outcome, diagnostic)) = result {
                assign(
                    &plan,
                    current,
                    &mut reports,
                    runner.report(outcome, Some(diagnostic)),
                    sources,
                    observer,
                )?;
            } else {
                break;
            }
        }
    }
}

fn replay_plan(
    sources: &[SourceRoute],
    recorder: &mut ExplorationRecorder,
) -> Result<Vec<PlanNode>> {
    let mut plan = vec![PlanNode::default()];
    for (index, source) in sources.iter().enumerate() {
        let mut node = 0;
        for action in source.report.actions.iter() {
            if let Some(expected) = &plan[node].expected {
                anyhow::ensure!(
                    expected.kind == action.kind
                        && expected.option_keys == action.option_keys
                        && expected.input_state == action.input_state,
                    "source routes disagree about their shared input boundary"
                );
            } else {
                plan[node].expected = Some(action.clone());
            }
            let existing = plan[node]
                .children
                .iter()
                .find(|(selected, _)| *selected == action.selected)
                .map(|(_, id)| *id);
            node = if let Some(existing) = existing {
                existing
            } else {
                let prefix = recorder.branch(plan[node].prefix, action.clone())?;
                let id = plan.len();
                plan.push(PlanNode {
                    prefix,
                    ..Default::default()
                });
                plan[node].children.push((action.selected, id));
                id
            };
        }
        plan[node].terminals.push(index);
    }
    Ok(plan)
}

fn assign(
    plan: &[PlanNode],
    node: usize,
    reports: &mut [Option<RunReport>],
    report: RunReport,
    sources: &[SourceRoute],
    observer: &mut Observer<'_>,
) -> Result<()> {
    let reached_prefix = plan[node].prefix;
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        for index in &plan[node].terminals {
            if reports[*index].is_some() {
                continue;
            }
            observer(&ExplorationEvent::Terminal {
                pass: ExplorationPass::Asb,
                requested_prefix: sources[*index].requested,
                reached_prefix,
                report: &report,
                boundary: sources[*index].boundary.as_ref().filter(|boundary| {
                    boundary.outcome == report.outcome
                        && sources[*index].report.actions == report.actions
                }),
            })?;
            reports[*index] = Some(report.clone());
        }
        pending.extend(plan[node].children.iter().map(|(_, id)| *id));
    }
    Ok(())
}

fn selection(decision: &Decision, selected: usize) -> ActionSelection {
    ActionSelection {
        kind: decision.kind,
        option_keys: decision.option_keys.clone(),
        input_state: decision.input_state.clone(),
        selected,
    }
}

fn capture_branch(
    runner: &mut Runner,
    choices: Vec<(usize, usize)>,
    decision: &Decision,
) -> Result<Branch> {
    let focus = runner
        .runtime
        .driver()
        .input_focus(&runner.presentation.bridge, runner.presentation.root)
        .context("checkpoint host focus is outside the authored stage")?;
    let directory = tempfile::tempdir_in(
        runner
            .saves
            .path()
            .parent()
            .context("checkpoint parent directory")?,
    )?;
    let checkpoint = runner
        .runtime
        .checkpoint()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let mut writer = BufWriter::new(File::create(directory.path().join("runtime.checkpoint"))?);
    checkpoint
        .write_to(&mut writer)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    writer.flush()?;
    fs::write(
        directory.path().join("host.state"),
        runner.host.state().to_le_bytes(),
    )?;
    let storage = directory.path().join("storage");
    fs::create_dir(&storage)?;
    copy_storage(runner.saves.path(), &storage)?;
    Ok(Branch {
        directory,
        actions: runner.actions.clone(),
        choices,
        next: 1,
        kind: decision.kind,
        keys: decision.option_keys.clone(),
        input_state: decision.input_state.clone(),
        steps: runner.steps,
        window: runner.window.clone(),
        focus,
    })
}

fn resume(
    runner: &mut Runner,
    frontier: &mut Frontier,
) -> Result<Option<(usize, Result<(), DriveFailure>)>> {
    let Some((key, mut branch)) = frontier.branches.pop_first() else {
        return Ok(None);
    };
    let result = (|| {
        let (selected, destination) = branch.choices[branch.next];
        branch.next += 1;
        if let Err(error) = restore(runner, branch.directory.path(), branch.window.clone()) {
            runner.actions = branch.actions.clone();
            return Ok(Some((
                destination,
                Err((
                    RunOutcome::CheckpointUnavailable,
                    RunDiagnostic {
                        message: format!("checkpoint restore unavailable: {error:#}"),
                        stack: Vec::new(),
                    },
                )),
            )));
        }
        runner.actions = branch.actions.clone();
        runner.steps = branch.steps;
        runner.presentation.restore_focus(branch.focus.clone());
        let decision = match runner.restored_decision() {
            Ok(Some(decision))
                if decision.kind == branch.kind
                    && decision.option_keys == branch.keys
                    && decision.input_state == branch.input_state =>
            {
                decision
            }
            Ok(actual) => {
                let actual_keys = actual
                    .as_ref()
                    .map_or(&[][..], |decision| decision.option_keys.as_ref());
                let added = actual_keys
                    .iter()
                    .filter(|key| !branch.keys.contains(key))
                    .collect::<Vec<_>>();
                let removed = branch
                    .keys
                    .iter()
                    .filter(|key| !actual_keys.contains(key))
                    .collect::<Vec<_>>();
                return Ok(Some((
                    destination,
                    Err(runner.failure(
                        RunOutcome::ReplayMismatch,
                        format!("restored input identities differ from their checkpoint: state={:?}, expected_kind={:?}, expected_targets={}, actual_kind={:?}, actual_targets={}, added={added:?}, removed={removed:?}", runner.runtime.driver().state(), branch.kind, branch.keys.len(), actual.as_ref().map(|decision|decision.kind), actual.as_ref().map_or(0,|decision|decision.option_keys.len())),
                    )),
                )));
            }
            Err((RunOutcome::Blocked, diagnostic)) => {
                return Ok(Some((
                    destination,
                    Err((RunOutcome::CheckpointUnavailable, diagnostic)),
                )));
            }
            Err(error) => return Ok(Some((destination, Err(error)))),
        };
        let result = runner.apply(&decision, selected);
        Ok(Some((destination, result)))
    })();
    if branch.next < branch.choices.len() {
        frontier.branches.insert(key, branch);
    }
    result
}

fn restore(
    runner: &mut Runner,
    directory: &Path,
    window: super::window::VirtualWindow,
) -> Result<()> {
    let wake = runner.wake.clone();
    runner
        .runtime
        .settle_checkpoint_storage(runner.logical_ns, || wait(&wake))
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    for entry in fs::read_dir(runner.saves.path())? {
        let entry = entry?;
        anyhow::ensure!(
            entry.file_type()?.is_file(),
            "unexpected save-storage entry"
        );
        fs::remove_file(entry.path())?;
    }
    copy_storage(&directory.join("storage"), runner.saves.path())?;
    let checkpoint = RuntimeCheckpoint::read_from(BufReader::new(File::open(
        directory.join("runtime.checkpoint"),
    )?))
    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let state: [u8; 8] = fs::read(directory.join("host.state"))?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid checkpoint host-environment state"))?;
    runner.host.set_state(u64::from_le_bytes(state));
    runner.window = window;
    runner.window.install(
        runner
            .runtime
            .driver_mut()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?,
    );
    runner
        .runtime
        .restore_checkpoint(checkpoint, runner.logical_ns, || wait(&wake))
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    runner.window.publish(
        runner
            .runtime
            .driver_mut()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?,
    );
    runner
        .reset_presentation()
        .map_err(|(_, error)| anyhow::anyhow!(error.message))
}

fn wait(wake: &reel_core::wake::WakeSignal) {
    if !wake.acknowledge() {
        std::thread::park();
    }
}

fn append_cleanup(report: &mut RunReport, (outcome, diagnostic): DriveFailure) {
    if let Some(existing) = &mut report.diagnostic {
        existing.message.push_str("; cleanup: ");
        existing.message.push_str(&diagnostic.message);
    } else {
        report.diagnostic = Some(diagnostic);
    }
    if matches!(
        report.outcome,
        RunOutcome::Finished
            | RunOutcome::UiCycle
            | RunOutcome::Blocked
            | RunOutcome::CheckpointUnavailable
    ) {
        report.outcome = outcome;
    }
}

fn copy_storage(source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        anyhow::ensure!(
            entry.file_type()?.is_file(),
            "unexpected save-storage entry"
        );
        fs::copy(entry.path(), destination.join(entry.file_name()))?;
    }
    Ok(())
}

fn terminal_report(runner: &Runner, result: Result<Option<Decision>, DriveFailure>) -> RunReport {
    match result {
        Ok(None) => runner.report(RunOutcome::Finished, None),
        Ok(Some(_)) => mismatch(runner, "execution reached an unexpected input boundary"),
        Err((outcome, diagnostic)) => runner.report(outcome, Some(diagnostic)),
    }
}

fn mismatch(runner: &Runner, message: &str) -> RunReport {
    runner.report(
        RunOutcome::ReplayMismatch,
        Some(RunDiagnostic {
            message: message.into(),
            stack: Vec::new(),
        }),
    )
}

fn checkpoint_failure(runner: &Runner, operation: &str, error: &anyhow::Error) -> RunReport {
    runner.report(
        RunOutcome::CheckpointUnavailable,
        Some(RunDiagnostic {
            message: format!("checkpoint {operation} unavailable: {error:#}"),
            stack: Vec::new(),
        }),
    )
}

fn cycle_report(runner: &Runner, repeated: &ObservedBoundary) -> RunReport {
    let storage = repeated.storage_change.as_ref().map_or_else(String::new, |(before, after)| {
        format!("; isolated persistent data changed (SHA-256 {before} -> {after}); this hidden-state difference remains unexplored")
    });
    let input = if repeated.input_changed {
        "; editable control values changed; the remaining value combinations are unexplored"
    } else {
        ""
    };
    runner.report(RunOutcome::UiCycle, Some(RunDiagnostic {
        message: format!("UI control observation was already reached at prefix {}; hidden business-state equivalence is not established{storage}{input}", repeated.first_prefix),
        stack: Vec::new(),
    }))
}

fn observe_boundary(
    runner: &Runner,
    decision: &Decision,
    prefix: usize,
    observed: &mut BTreeMap<String, Observation>,
) -> Result<Option<ObservedBoundary>> {
    if decision.kind != ActionKind::Controls {
        return Ok(None);
    }
    let fingerprint = fingerprint(runner, decision)?;
    if let Some(first) = observed.get(&fingerprint.visible) {
        Ok(Some(ObservedBoundary {
            fingerprint: fingerprint.exact,
            first_prefix: first.first_prefix,
            option_keys: decision.option_keys.clone(),
            storage_change: (first.storage != fingerprint.storage)
                .then(|| (first.storage.clone(), fingerprint.storage)),
            input_changed: first.input_state != decision.input_state,
        }))
    } else {
        observed.insert(
            fingerprint.visible,
            Observation {
                first_prefix: prefix,
                storage: fingerprint.storage,
                input_state: decision.input_state.clone(),
            },
        );
        Ok(None)
    }
}

fn control_fingerprint(runner: &Runner, decision: &Decision) -> Result<String> {
    Ok(fingerprint(runner, decision)?.exact)
}

fn fingerprint(runner: &Runner, decision: &Decision) -> Result<Fingerprint> {
    let mut hash = Sha256::new();
    for word in runner.window.fingerprint() {
        part(&mut hash, &word.to_le_bytes());
    }
    for key in decision.option_keys.iter() {
        part(&mut hash, key.as_bytes());
        if decision.kind == ActionKind::Controls {
            control_content(&mut hash, runner.runtime.driver().adapter().stage(), key)?;
        }
    }
    let vm = runner.runtime.driver().adapter().vm();
    let mut waits = Vec::new();
    for action in &decision.actions {
        if let super::DecisionAction::WaitForActivities(finite) = action {
            for &wait in finite {
                if let Some(sites) = vm.wait_call_sites(wait) {
                    let mut chain = Sha256::new();
                    for site in sites {
                        part(&mut chain, &site.code.function.0.to_le_bytes());
                        part(&mut chain, &site.code.version.to_le_bytes());
                        part(&mut chain, &site.pc.0.to_le_bytes());
                    }
                    waits.push(encode_hash(chain));
                }
            }
        }
    }
    waits.sort_unstable();
    for wait in waits {
        part(&mut hash, wait.as_bytes());
    }
    let session = runner.runtime.driver().ui().session();
    let mut read = session.read_progress().ids().collect::<Vec<_>>();
    read.sort_unstable();
    for id in read {
        part(&mut hash, id.as_bytes());
    }
    if let Some(group) = session.view().history.last() {
        for line in group.lines() {
            part(&mut hash, line.speaker_id().unwrap_or("").as_bytes());
            part(&mut hash, line.resolved_text().as_bytes());
        }
    }
    let mut files = fs::read_dir(runner.saves.path())?.collect::<std::io::Result<Vec<_>>>()?;
    files.sort_by_key(|entry| entry.file_name());
    let mut scratch = [0_u8; 64 * 1024];
    let mut storage = Sha256::new();
    for entry in files {
        let path = entry.path();
        match path.extension().and_then(|extension| extension.to_str()) {
            // DirectorySaves names slots with blake3(slot).save. Only slot
            // identity and existence belong to this observed UI fingerprint:
            // saved_at and the execution snapshot advance on every save.
            Some("save") => {
                part(&mut hash, entry.file_name().as_encoded_bytes());
                continue;
            }
            // Hidden persistent values remain part of exact replay validation,
            // separately from visible UI observation reuse.
            Some("data") => {}
            _ => continue,
        }
        part(&mut storage, entry.file_name().as_encoded_bytes());
        let mut file = File::open(path)?;
        loop {
            let read = file.read(&mut scratch)?;
            if read == 0 {
                break;
            }
            storage.update(&scratch[..read]);
        }
    }
    let visible = encode_hash(hash);
    let storage = encode_hash(storage);
    let mut exact = Sha256::new();
    part(&mut exact, visible.as_bytes());
    part(&mut exact, storage.as_bytes());
    if let Some(state) = &decision.input_state {
        part(&mut exact, state.as_bytes());
    }
    Ok(Fingerprint {
        visible,
        storage,
        exact: encode_hash(exact),
    })
}

fn encode_hash(hash: Sha256) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(64);
    for byte in hash.finalize() {
        write!(encoded, "{byte:02x}").expect("formatting into String");
    }
    encoded
}

fn control_content(hash: &mut Sha256, stage: &reel_ui::stage::Stage, key: &str) -> Result<()> {
    if key.starts_with('@') {
        return Ok(());
    }
    let (path, _) = key
        .split_once(':')
        .context("control key has no authored path")?;
    let mut indices = path.split('/');
    anyhow::ensure!(indices.next() == Some("0"), "control path has no root");
    let mut node = stage
        .root()
        .and_then(|root| stage.node(root))
        .context("control tree has no root")?;
    for index in indices {
        node = index
            .parse::<usize>()
            .ok()
            .and_then(|index| node.children.get(index))
            .and_then(|id| stage.node(*id))
            .context("control path differs from the accepted stage")?;
    }
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if !node.props.visible {
            continue;
        }
        if let reel_ui::stage::NodeKind::Text(text) = &node.kind {
            part(hash, b"text");
            part(hash, text.plain().as_bytes());
        }
        if let reel_ui::stage::NodeKind::Image { source, .. } = &node.kind {
            part(hash, b"image");
            if let Some(bytes) = source.bytes() {
                part(hash, b"encoded");
                part(hash, &Sha256::digest(bytes));
            } else if let Some(lease) = source.lease() {
                // Resource handles, video frame versions and external tokens
                // identify producers or samples, not stable authored content.
                match lease.data() {
                    reel_ui::resources::ResourceData::Image { .. } => part(hash, b"decoded"),
                    reel_ui::resources::ResourceData::Yuv420(_) => part(hash, b"video"),
                    _ => part(hash, b"external"),
                }
            } else {
                part(hash, b"asset");
                part(hash, source.key().as_bytes());
            }
        }
        for id in node.children.iter().rev() {
            pending.push(stage.node(*id).context("control text child was retired")?);
        }
    }
    Ok(())
}

fn part(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestStatus;

    fn check(source: &str) -> Result<ExplorationReport> {
        check_observed(source, |_| Ok(()))
    }

    fn check_observed(
        source: &str,
        observer: impl FnMut(&ExplorationEvent<'_>) -> Result<()>,
    ) -> Result<ExplorationReport> {
        let source =
            reel_engine::aside::source::compile_story(source, "checkpoint_test", 1, "main")
                .expect("compile checkpoint scenario");
        let asb = bytecode_story(&source)?;
        let resources = tempfile::tempdir()?;
        explore_with_observer(
            source,
            asb,
            resources.path(),
            &RunEnvironment {
                seed: 17,
                unix_seconds: 42,
            },
            observer,
        )
    }

    fn bytecode_story(source: &CompiledStory) -> Result<CompiledStory> {
        let limits = aside_vm::bytecode::Limits::default();
        let encoded = aside_vm::bytecode::encode(source.program.package(), limits)?;
        Ok(CompiledStory {
            program: aside_vm::bytecode::VerifiedProgram::decode(&encoded, limits)?,
            initializer: source.initializer,
            entry: source.entry,
            catalogs: Default::default(),
        })
    }

    #[test]
    fn sibling_menu_routes_reuse_observations_and_keep_first_encounter_actions() -> Result<()> {
        let mut source_terminals = Vec::new();
        let report = check_observed(
            r#"
            fn main() {
                game.configure(width:160,height:160);
                let state = model({panel:false});
                stage.replace(view(|| if state.panel {
                    column(width:160,height:160) {
                        button("Stay", width:160,height:48, on_click:|| {});
                        button("Return", width:160,height:48, on_click:|| { state.panel=false; });
                    }
                } else {
                    column(width:160,height:160) {
                        button("Open A", width:160,height:48, on_click:|| { state.panel=true; });
                        button("Open B", width:160,height:48, on_click:|| { state.panel=true; });
                        button("Exit", width:160,height:48, on_click:|| game.exit());
                    }
                }));
            }
            "#,
            |event| {
                if let ExplorationEvent::Terminal {
                    pass: ExplorationPass::Source,
                    report,
                    ..
                } = event
                {
                    source_terminals.push(
                        report
                            .actions
                            .iter()
                            .map(|action| action.selected)
                            .collect::<Vec<_>>(),
                    );
                }
                Ok(())
            },
        )?;
        assert_eq!(report.status, TestStatus::Incomplete, "{report:?}");
        assert_eq!(
            source_terminals,
            vec![vec![1], vec![2], vec![0, 0], vec![0, 1]]
        );
        assert_eq!(report.runs.len(), 4);
        assert!(report.runs.iter().all(|run| run.replay_mismatch.is_none()));
        assert!(
            report
                .runs
                .iter()
                .filter(|run| run.source.outcome != RunOutcome::Finished)
                .all(|run| run.source.outcome == RunOutcome::UiCycle
                    && run.asb.outcome == RunOutcome::UiCycle)
        );
        assert_eq!(report.runs[1].source.outcome, RunOutcome::Finished);
        assert_eq!(report.runs[1].asb.outcome, RunOutcome::Finished);
        Ok(())
    }

    #[test]
    fn visible_custom_control_text_keeps_successive_rounds_actionable() -> Result<()> {
        let report = check(
            r#"
            fn main() {
                game.configure(width:160,height:64);
                let state = model({round:0});
                let next = button(width:160,height:64,on_click:|| {
                    state.round += 1;
                    if state.round == 3 { game.exit(); }
                });
                next.add(text(derive(|| f"Round {state.round}")));
                stage.replace(next);
            }
        "#,
        )?;
        assert_eq!(report.status, TestStatus::Passed, "{report:?}");
        assert_eq!(report.runs.len(), 1);
        assert_eq!(report.runs[0].source.actions.len(), 3);
        assert!(report.runs[0].replay_mismatch.is_none());
        let keys = &report.runs[0].source.actions[0].option_keys;
        assert!(
            report.runs[0]
                .source
                .actions
                .iter()
                .all(|action| &action.option_keys == keys)
        );
        Ok(())
    }

    #[test]
    fn shallow_menu_actions_run_before_deeper_configuration_paths() -> Result<()> {
        let mut paths = Vec::new();
        let report = check_observed(
            r#"
            fn main() {
                game.configure(width:160,height:100);
                let state = model({setting:0});
                let next = button(width:160,height:48,on_click:|| {
                    if state.setting == 12 { game.exit(); }
                    else { state.setting += 1; }
                });
                next.add(text(derive(|| f"Setting {state.setting}")));
                stage.replace(column(width:160,height:100) {
                    next;
                    button("Start",width:160,height:48,on_click:|| game.exit());
                });
            }
        "#,
            |event| {
                if let ExplorationEvent::Terminal {
                    pass: ExplorationPass::Source,
                    report,
                    ..
                } = event
                {
                    paths.push(
                        report
                            .actions
                            .iter()
                            .map(|action| action.selected)
                            .collect::<Vec<_>>(),
                    );
                }
                Ok(())
            },
        )?;
        assert_eq!(report.status, TestStatus::Passed, "{report:?}");
        assert_eq!(paths[0], vec![1]);
        assert_eq!(paths[1], vec![0, 1]);
        assert_eq!(paths.len(), 14);
        assert!(paths.windows(2).all(|pair| pair[0].len() <= pair[1].len()));
        assert!(report.runs.iter().all(|run| run.replay_mismatch.is_none()));
        Ok(())
    }

    #[test]
    fn asb_setup_failure_keeps_actual_source_faults_and_action_paths() -> Result<()> {
        let source = reel_engine::aside::source::compile_story(
            r#"fn main() {
                stage.replace(stack());
                let route = choose([{key:"fault",label:"Fault"},{key:"safe",label:"Safe"}]);
                if route == "fault" { throw "source fault before ASB setup"; }
            }"#,
            "checkpoint_test",
            1,
            "main",
        )?;
        let asb = CompiledStory {
            program: source.program.clone(),
            initializer: aside_vm::bytecode::FunctionId(u32::MAX),
            entry: source.entry,
            catalogs: Default::default(),
        };
        let resources = tempfile::tempdir()?;
        let report = explore_with_environment(
            source,
            asb,
            resources.path(),
            &RunEnvironment {
                seed: 17,
                unix_seconds: 42,
            },
        )?;
        assert_eq!(report.status, TestStatus::Failed);
        assert_eq!(report.runs.len(), 2);
        assert_eq!(report.runs[0].source.outcome, RunOutcome::RuntimeFailure);
        assert!(
            report.runs[0]
                .source
                .diagnostic
                .as_ref()
                .unwrap()
                .message
                .contains("source fault before ASB setup")
        );
        assert_eq!(report.runs[0].source.actions[0].selected, 0);
        assert_eq!(report.runs[1].source.outcome, RunOutcome::Finished);
        assert!(report.runs.iter().all(|run| {
            run.asb.outcome == RunOutcome::ReplayMismatch
                && run
                    .asb
                    .diagnostic
                    .as_ref()
                    .unwrap()
                    .message
                    .contains("ASB exploration could not continue")
        }));
        Ok(())
    }

    #[test]
    fn nested_sibling_checkpoints_restore_virtual_window_state() -> Result<()> {
        let report = check(
            r#"
            fn main() {
                game.configure(width:160,height:100);
                stage.replace(stack());
                let initial = window.info();
                let first = choose([{key:"left",label:"Left"},{key:"right",label:"Right"}]);
                if window.info().width != initial.width { throw "sibling window width leaked"; }
                if window.info().fullscreen != initial.fullscreen { throw "sibling fullscreen leaked"; }
                await window.configure(width:500,height:300,vsync:false);
                let second = choose([{key:"a",label:"A"},{key:"b",label:"B"}]);
                let restored = window.info();
                if restored.width != 500 || restored.height != 300 || restored.vsync {
                    throw "nested branch window state was lost";
                }
                await window.configure(fullscreen:true,vsync:true);
            }
            "#,
        )?;
        assert_eq!(report.status, TestStatus::Passed, "{report:?}");
        assert_eq!(report.runs.len(), 4);
        assert!(report.runs.iter().all(|run| run.replay_mismatch.is_none()));
        Ok(())
    }

    #[test]
    fn nested_checkpoints_preserve_branch_local_persistent_files_and_vm_values() -> Result<()> {
        let report = check(
            r#"
            fn main() {
                stage.replace(stack());
                let first = choose([{key:"left",label:"Left"},{key:"right",label:"Right"}]);
                if await persistent.read("branch", default:null) != null {
                    throw "previous sibling persistent state leaked";
                }
                await persistent.write("branch", first);
                let second = choose([{key:"a",label:"A"},{key:"b",label:"B"}]);
                if await persistent.read("branch") != first { throw "parent branch state was lost"; }
                if await persistent.read("leaf", default:null) != null { throw "previous leaf leaked"; }
                await persistent.write("leaf", second);
            }
        "#,
        )?;
        assert_eq!(report.status, TestStatus::Passed, "{report:?}");
        assert_eq!(report.runs.len(), 4);
        assert_eq!(report.reached.len(), 4);
        assert_eq!(report.prefixes.len(), 7);
        assert!(report.pending.is_empty());
        assert!(report.runs.iter().all(|run| run.source.actions.len() == 2));
        Ok(())
    }

    #[test]
    fn a_faulted_leaf_preserves_its_diagnostic_while_later_siblings_execute() -> Result<()> {
        let report = check(
            r#"
            fn main() {
                stage.replace(stack());
                let first = choose([{key:"left",label:"Left"},{key:"right",label:"Right"}]);
                let second = choose([{key:"a",label:"A"},{key:"b",label:"B"}]);
                if first == "left" && second == "a" { throw "original route fault"; }
            }
        "#,
        )?;
        assert_eq!(report.status, TestStatus::Failed, "{report:?}");
        assert_eq!(report.runs.len(), 4);
        assert!(report.pending.is_empty());
        assert!(report.runs.iter().all(|run| run.replay_mismatch.is_none()));
        let first = &report.runs[0];
        for run in [&first.source, &first.asb] {
            assert_eq!(run.outcome, RunOutcome::RuntimeFailure);
            let diagnostic = run.diagnostic.as_ref().expect("original route diagnostic");
            assert!(diagnostic.message.contains("original route fault"));
            assert_eq!(diagnostic.stack[0].path.as_deref(), Some("checkpoint_test"));
            assert_eq!(diagnostic.stack[0].line, Some(6));
        }
        assert!(report.runs[1..].iter().all(|run| {
            run.source.outcome == RunOutcome::Finished && run.asb.outcome == RunOutcome::Finished
        }));
        Ok(())
    }

    #[test]
    fn ui_self_loop_is_observed_and_remaining_exit_button_is_explored() -> Result<()> {
        let report = check(
            r#"
            fn main() {
                game.configure(width:160,height:100);
                stage.replace(column(width:160,height:100) {
                    button("Remain", width:160,height:48, on_click:|| {});
                    button("Exit", width:160,height:48, on_click:|| game.exit());
                });
            }
        "#,
        )?;
        assert_eq!(report.status, TestStatus::Incomplete, "{report:?}");
        assert!(
            report
                .runs
                .iter()
                .any(|run| run.source.outcome == RunOutcome::UiCycle)
        );
        assert!(
            report
                .runs
                .iter()
                .any(|run| run.source.outcome == RunOutcome::Finished)
        );
        assert!(!report.pending.is_empty());
        Ok(())
    }

    #[test]
    fn saving_the_same_slot_returns_to_an_observed_menu_and_preserves_exit_routes() -> Result<()> {
        let report = check(
            r#"
            fn main() {
                game.configure(width:160,height:100);
                let state = model({saves:0});
                stage.replace(column(width:160,height:100) {
                    button("Save", width:160,height:48, on_click:|| {
                        state.saves += 1;
                        if state.saves > 2 { throw "same-slot saves escaped observed UI cycle"; }
                        await saves.save("quick");
                    });
                    button("Exit", width:160,height:48, on_click:|| {
                        let slots = await saves.list();
                        if state.saves == 0 {
                            if slots.len() != 0 { throw "sibling save leaked into entry"; }
                        } else {
                            if slots.len() != 1 { throw "same slot was not committed"; }
                            if slots[0].id != "quick" { throw "wrong slot identity"; }
                            if slots[0].saved_at != 42 { throw "save timestamp ignored the injected civil time"; }
                        }
                        game.exit();
                    });
                });
            }
        "#,
        )?;
        assert_eq!(report.status, TestStatus::Incomplete, "{report:?}");
        assert_eq!(report.runs.len(), 3, "{report:?}");
        assert!(report.runs.iter().all(|run| run.replay_mismatch.is_none()));
        let cycle = report
            .runs
            .iter()
            .find(|run| run.source.outcome == RunOutcome::UiCycle)
            .expect("same-slot save must return to an observed UI state");
        assert_eq!(cycle.source.actions.len(), 2);
        assert_eq!(cycle.asb.outcome, RunOutcome::UiCycle);
        let finished = report
            .runs
            .iter()
            .filter(|run| run.source.outcome == RunOutcome::Finished)
            .map(|run| {
                run.source
                    .actions
                    .iter()
                    .map(|action| action.selected)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert!(finished.contains(&vec![0, 1]), "{report:?}");
        assert!(finished.contains(&vec![1]), "{report:?}");
        assert!(!report.pending.is_empty());
        Ok(())
    }

    #[test]
    fn hidden_persistent_progress_is_incomplete_but_its_explicit_trace_finishes() -> Result<()> {
        let source = r#"
            fn main() {
                game.configure(width:160,height:100);
                stage.replace(column(width:160,height:100) {
                    button("Continue", width:160,height:48, on_click:|| {
                        let value = await persistent.read("progress", default:0);
                        if value == 2 { game.exit(); }
                        else { await persistent.write("progress", value + 1); }
                    });
                    button("Exit", width:160,height:48, on_click:|| game.exit());
                });
            }
            "#;
        let report = check(source)?;
        assert_eq!(report.status, TestStatus::Incomplete, "{report:?}");
        assert_eq!(report.runs.len(), 2);
        let incomplete = report
            .runs
            .iter()
            .find(|run| run.source.outcome == RunOutcome::UiCycle)
            .expect("changed hidden persistent data remains an unresolved path");
        assert_eq!(incomplete.source.actions.len(), 1);
        assert!(
            incomplete
                .source
                .diagnostic
                .as_ref()
                .unwrap()
                .message
                .contains("persistent data changed")
        );
        assert!(incomplete.replay_mismatch.is_none());
        assert!(!report.pending.is_empty());
        let explicit = vec![incomplete.source.actions[0].clone(); 3];
        let environment = RunEnvironment {
            seed: 17,
            unix_seconds: 42,
        };
        let resources = tempfile::tempdir()?;
        let story =
            reel_engine::aside::source::compile_story(source, "checkpoint_test", 1, "main")?;
        let asb = bytecode_story(&story)?;
        for story in [story, asb] {
            let exact = super::super::run(
                story,
                resources.path(),
                &super::super::RunOptions {
                    strict_replay: true,
                    environment: environment.clone(),
                },
                &explicit,
            )?;
            assert_eq!(exact.outcome, RunOutcome::Finished, "{exact:?}");
            assert_eq!(exact.actions.len(), 3);
        }
        Ok(())
    }

    #[test]
    fn restoring_siblings_restores_the_host_entropy_cursor_before_new_streams() -> Result<()> {
        let report = check(
            r#"
            fn main() {
                stage.replace(stack());
                let first = choose([{key:"left",label:"Left"},{key:"right",label:"Right"}]);
                let stream = random();
                let key = f"sample-{stream.int(0, 2147483647)}";
                choose([{key:key,label:"Sample"},{key:"fixed",label:"Fixed"}]);
            }
        "#,
        )?;
        assert_eq!(report.status, TestStatus::Passed, "{report:?}");
        assert_eq!(report.runs.len(), 4);
        let expected = &report.runs[0].source.actions[1].option_keys;
        assert!(
            report
                .runs
                .iter()
                .all(|run| &run.source.actions[1].option_keys == expected)
        );
        Ok(())
    }
}
