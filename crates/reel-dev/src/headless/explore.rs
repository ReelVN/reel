//! Depth-first exploration using temporary production session checkpoints.

use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::Path,
};

use anyhow::{Context, Result};
use reel_engine::aside::source::CompiledStory;
use reel_player::story_runtime::RuntimeCheckpoint;
use sha2::{Digest, Sha256};

use super::{
    ActionKind, ActionSelection, Decision, DriveFailure, RunDiagnostic, RunEnvironment, RunOutcome,
    RunReport, Runner,
};
use crate::testing::{ExplorationRecorder, ExplorationReport, RunPair};

struct Branch {
    directory: tempfile::TempDir,
    parent_prefix: usize,
    choices: Vec<(usize, usize)>,
    next: usize,
    kind: ActionKind,
    keys: Vec<String>,
    observed_depth: usize,
    steps: u64,
}

struct SourceRoute {
    requested: usize,
    report: RunReport,
}

#[derive(Default)]
struct PlanNode {
    prefix: usize,
    expected: Option<(usize, usize)>,
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
    let mut recorder = ExplorationRecorder::new();
    let source_routes = source_pass(source, resources, &mut recorder, environment)?;
    // source_pass cancels and drops all live state before constructing ASB.
    let asb_reports = asb_pass(asb, resources, &source_routes, &mut recorder, environment)?;
    for (source, asb) in source_routes.into_iter().zip(asb_reports) {
        recorder.record(
            source.requested,
            RunPair {
                source: source.report,
                asb,
            },
        )?;
    }
    Ok(recorder.finish())
}

fn source_pass(
    story: CompiledStory,
    resources: &Path,
    recorder: &mut ExplorationRecorder,
    environment: &RunEnvironment,
) -> Result<Vec<SourceRoute>> {
    let mut runner = Runner::new_with_environment(story, resources, environment)?;
    let mut routes = Vec::new();
    let mut frontier = Vec::new();
    let mut observed = Vec::new();
    let mut current = 0;
    let mut requested = 0;
    loop {
        let result = runner.drive();
        match result {
            Ok(Some(decision)) => {
                if observe_cycle(&runner, &decision, &mut observed)? {
                    routes.push(SourceRoute {
                        requested,
                        report: cycle_report(&runner),
                    });
                } else {
                    anyhow::ensure!(
                        !decision.option_keys.is_empty(),
                        "input decision has no targets"
                    );
                    let choices = (0..decision.option_keys.len())
                        .map(|selected| {
                            recorder
                                .branch(current, selection(&decision, selected))
                                .map(|id| (selected, id))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let checkpoint = if choices.len() > 1 {
                        capture_branch(
                            &mut runner,
                            current,
                            choices.clone(),
                            &decision,
                            observed.len(),
                        )
                        .map(|branch| frontier.push(branch))
                    } else {
                        Ok(())
                    };
                    if let Err(error) = checkpoint {
                        routes.push(SourceRoute {
                            requested,
                            report: checkpoint_failure(&runner, "capture", &error),
                        });
                    } else {
                        current = choices[0].1;
                        match runner.apply(&decision, choices[0].0) {
                            Ok(()) => continue,
                            Err((outcome, diagnostic)) => routes.push(SourceRoute {
                                requested,
                                report: runner.report(outcome, Some(diagnostic)),
                            }),
                        }
                    }
                }
            }
            terminal => routes.push(SourceRoute {
                requested,
                report: terminal_report(&runner, terminal),
            }),
        }
        loop {
            let Some((destination, result)) =
                resume(&mut runner, &mut frontier, &mut observed, recorder)?
            else {
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
                routes.push(SourceRoute {
                    requested,
                    report: runner.report(outcome, Some(diagnostic)),
                });
            } else {
                break;
            }
        }
    }
}

fn asb_pass(
    story: CompiledStory,
    resources: &Path,
    sources: &[SourceRoute],
    recorder: &mut ExplorationRecorder,
    environment: &RunEnvironment,
) -> Result<Vec<RunReport>> {
    let plan = replay_plan(sources, recorder)?;
    let mut reports = vec![None; sources.len()];
    let mut runner = Runner::new_with_environment(story, resources, environment)?;
    let mut frontier = Vec::new();
    let mut observed = Vec::new();
    let mut current = 0;
    loop {
        let result = runner.drive();
        let node = &plan[current];
        match result {
            Ok(Some(decision)) => {
                for index in &node.terminals {
                    if sources[*index].report.outcome == RunOutcome::CheckpointUnavailable {
                        reports[*index] = Some(runner.report(RunOutcome::CheckpointUnavailable, Some(RunDiagnostic {
                            message:"ASB reached a boundary whose source checkpoint was unavailable".into(),
                            stack:Vec::new(),
                        })));
                    }
                }
                let cycle = observe_cycle(&runner, &decision, &mut observed)?;
                if node.expected.is_none() {
                    let report = if node.terminals.iter().any(|index| {
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
                    } else if cycle {
                        cycle_report(&runner)
                    } else {
                        mismatch(
                            &runner,
                            "ASB reached an additional input boundary after the recorded route",
                        )
                    };
                    assign(&plan, current, &mut reports, report);
                } else if cycle {
                    assign(&plan, current, &mut reports, cycle_report(&runner));
                } else if node.expected.is_some_and(|(source, action)| {
                    let expected = &sources[source].report.actions[action];
                    expected.kind != decision.kind || expected.option_keys != decision.option_keys
                }) {
                    assign(
                        &plan,
                        current,
                        &mut reports,
                        mismatch(
                            &runner,
                            "ASB input target identities differ from the recorded source route",
                        ),
                    );
                } else {
                    anyhow::ensure!(
                        !node.children.is_empty(),
                        "replay plan has an input without a successor"
                    );
                    let checkpoint = if node.children.len() > 1 {
                        capture_branch(
                            &mut runner,
                            node.prefix,
                            node.children.clone(),
                            &decision,
                            observed.len(),
                        )
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
                        );
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
                                );
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
            ),
        }
        loop {
            let Some((destination, result)) =
                resume(&mut runner, &mut frontier, &mut observed, recorder)?
            else {
                if let Err(error) = runner.close()
                    && let Some(report) = reports.iter_mut().rev().flatten().next()
                {
                    append_cleanup(report, error);
                }
                return reports
                    .into_iter()
                    .map(|report| report.context("ASB replay route was not executed"))
                    .collect();
            };
            current = destination;
            if let Err((outcome, diagnostic)) = result {
                assign(
                    &plan,
                    current,
                    &mut reports,
                    runner.report(outcome, Some(diagnostic)),
                );
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
        for (action_index, action) in source.report.actions.iter().enumerate() {
            if let Some((expected_source, expected_action)) = plan[node].expected {
                let expected = &sources[expected_source].report.actions[expected_action];
                anyhow::ensure!(
                    expected.kind == action.kind && expected.option_keys == action.option_keys,
                    "source routes disagree about their shared input boundary"
                );
            } else {
                plan[node].expected = Some((index, action_index));
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

fn assign(plan: &[PlanNode], node: usize, reports: &mut [Option<RunReport>], report: RunReport) {
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        for index in &plan[node].terminals {
            reports[*index] = Some(report.clone());
        }
        pending.extend(plan[node].children.iter().map(|(_, id)| *id));
    }
}

fn selection(decision: &Decision, selected: usize) -> ActionSelection {
    ActionSelection {
        kind: decision.kind,
        option_keys: decision.option_keys.clone(),
        selected,
    }
}

fn capture_branch(
    runner: &mut Runner,
    parent_prefix: usize,
    choices: Vec<(usize, usize)>,
    decision: &Decision,
    observed_depth: usize,
) -> Result<Branch> {
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
        parent_prefix,
        choices,
        next: 1,
        kind: decision.kind,
        keys: decision.option_keys.clone(),
        observed_depth,
        steps: runner.steps,
    })
}

fn resume(
    runner: &mut Runner,
    frontier: &mut Vec<Branch>,
    observed: &mut Vec<String>,
    recorder: &ExplorationRecorder,
) -> Result<Option<(usize, Result<(), DriveFailure>)>> {
    loop {
        let Some(branch) = frontier.last_mut() else {
            return Ok(None);
        };
        if branch.next == branch.choices.len() {
            frontier.pop();
            continue;
        }
        let (selected, destination) = branch.choices[branch.next];
        branch.next += 1;
        let parent_actions = recorder.prefix(branch.parent_prefix)?;
        if let Err(error) = restore(runner, branch.directory.path()) {
            runner.actions = parent_actions;
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
        runner.actions = parent_actions;
        runner.steps = branch.steps;
        observed.truncate(branch.observed_depth);
        let decision = match runner.drive() {
            Ok(Some(decision))
                if decision.kind == branch.kind && decision.option_keys == branch.keys =>
            {
                decision
            }
            Ok(_) => {
                return Ok(Some((
                    destination,
                    Err(runner.failure(
                        RunOutcome::ReplayMismatch,
                        "restored input identities differ from their checkpoint",
                    )),
                )));
            }
            Err(error) => return Ok(Some((destination, Err(error)))),
        };
        let result = runner.apply(&decision, selected);
        return Ok(Some((destination, result)));
    }
}

fn restore(runner: &mut Runner, directory: &Path) -> Result<()> {
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
    runner
        .runtime
        .restore_checkpoint(checkpoint, runner.logical_ns, || wait(&wake))
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
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

fn cycle_report(runner: &Runner) -> RunReport {
    runner.report(RunOutcome::UiCycle,Some(RunDiagnostic {
        message:"an observed UI control state repeated; hidden business-state equivalence is not established".into(),
        stack:Vec::new(),
    }))
}

fn observe_cycle(runner: &Runner, decision: &Decision, observed: &mut Vec<String>) -> Result<bool> {
    if decision.kind != ActionKind::Controls {
        return Ok(false);
    }
    let fingerprint = control_fingerprint(runner, decision)?;
    if observed.contains(&fingerprint) {
        Ok(true)
    } else {
        observed.push(fingerprint);
        Ok(false)
    }
}

fn control_fingerprint(runner: &Runner, decision: &Decision) -> Result<String> {
    let mut hash = Sha256::new();
    for key in &decision.option_keys {
        part(&mut hash, key.as_bytes());
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
            // DirectorySaves writes persistent business values as REELDATA v1
            // in blake3(key).data; retain changes to those values in the key.
            Some("data") => {}
            _ => continue,
        }
        part(&mut hash, entry.file_name().as_encoded_bytes());
        let mut file = File::open(path)?;
        loop {
            let read = file.read(&mut scratch)?;
            if read == 0 {
                break;
            }
            hash.update(&scratch[..read]);
        }
    }
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(64);
    for byte in hash.finalize() {
        write!(encoded, "{byte:02x}").expect("formatting into String");
    }
    Ok(encoded)
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
        let source =
            reel_engine::aside::source::compile_story(source, "checkpoint_test", 1, "main")
                .expect("compile checkpoint scenario");
        let asb = CompiledStory {
            program: source.program.clone(),
            initializer: source.initializer,
            entry: source.entry,
            catalogs: Default::default(),
        };
        let resources = tempfile::tempdir()?;
        explore_with_environment(
            source,
            asb,
            resources.path(),
            &RunEnvironment {
                seed: 17,
                unix_seconds: 42,
            },
        )
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
    fn changed_persistent_business_values_keep_an_unchanged_menu_actionable() -> Result<()> {
        let report = check(
            r#"
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
        "#,
        )?;
        assert_eq!(report.status, TestStatus::Passed, "{report:?}");
        assert_eq!(report.runs.len(), 4);
        assert!(
            report
                .runs
                .iter()
                .all(|run| run.source.outcome == RunOutcome::Finished)
        );
        assert!(report.runs.iter().any(|run| {
            run.source
                .actions
                .iter()
                .map(|action| action.selected)
                .collect::<Vec<_>>()
                == vec![0, 0, 0]
        }));
        assert!(report.pending.is_empty());
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
