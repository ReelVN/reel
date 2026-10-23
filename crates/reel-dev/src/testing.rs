//! Shared action paths and source/ASB replay reports for automatic game checks.
//!
//! Prefixes share parent links. Reports retain action identities and diagnostics;
//! runtime checkpoints and their storage stay in the execution backend.

use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::headless::{ActionKind, ActionSelection, RunEnvironment, RunOutcome, RunReport};

/// Current report and playback trace format.
pub const SCHEMA_VERSION: u32 = 1;

/// Independent execution pass supplying an exploration event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplorationPass {
    /// Source-compiled program discovering accepted input paths.
    Source,
    /// Independently loaded ASB following the source's exact input paths.
    Asb,
}

/// Incremental observations emitted before a complete exploration report exists.
///
/// An observer may persist these borrowed values immediately. Terminal reports
/// contain their exact action paths; decisions describe accepted input only.
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ExplorationEvent<'a> {
    /// A fresh independent runtime is about to start this pass.
    PassStarted {
        /// Runtime and playback mode.
        pass: ExplorationPass,
    },
    /// Bounded progress sample during a long stretch without input decisions.
    Progress {
        /// Runtime and playback mode.
        pass: ExplorationPass,
        /// Execution steps on the currently running path.
        steps: u64,
        /// Current virtual host-clock sample, in nanoseconds.
        logical_ns: u64,
        /// CPU frames accepted during this runtime pass.
        accepted_frames: u64,
        /// Active narrative interaction protocol, without narrative text.
        interaction: Option<&'a str>,
        /// Generation of the active interaction in its current session.
        interaction_generation: Option<u64>,
        /// Stable content ID of the last active dialogue line.
        content_id: Option<&'a str>,
        /// Distinct completed content IDs on the current path.
        read_content: usize,
    },
    /// Execution reached an accepted input boundary.
    Decision {
        /// Runtime and playback mode.
        pass: ExplorationPass,
        /// Current node in the exploration's shared prefix table.
        prefix: usize,
        /// Number of submitted actions leading to this boundary.
        depth: usize,
        /// Input protocol whose ordered targets follow.
        kind: ActionKind,
        /// Exact accepted editable values used to validate this action boundary.
        input_state: Option<&'a str>,
        /// Complete ordered identities offered by the accepted frame.
        option_keys: &'a [String],
        /// Distinct control observations retained across source branches.
        observed_states: usize,
        /// Saved boundaries with alternatives awaiting execution.
        frontier: usize,
    },
    /// One source action extends the shared prefix table without copying a path.
    Branch {
        /// Accepted boundary supplying this action's kind and ordered targets.
        parent_prefix: usize,
        /// Shared prefix produced by submitting this action.
        prefix: usize,
        /// Index into the parent decision's ordered target keys.
        selected: usize,
    },
    /// One exact execution path reached a terminal classification.
    Terminal {
        /// Runtime and playback mode.
        pass: ExplorationPass,
        /// Prefix selected when this execution resumed from a checkpoint.
        requested_prefix: usize,
        /// Last submitted prefix before the terminal classification.
        reached_prefix: usize,
        /// Actual path, terminal result and original source diagnostic.
        report: &'a RunReport,
        /// Exact observed boundary for an incomplete route, when available.
        boundary: Option<&'a ReplayBoundary>,
    },
}

/// Stable playback inputs for the current game.
///
/// The digest identifies its deterministic compiled source package. Each action
/// records the kind, complete ordered target keys and selected index. Runtime
/// session IDs, wall timing, narrative payloads and VM snapshots are omitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayTrace {
    /// Playback format, currently SCHEMA_VERSION.
    pub schema_version: u32,
    /// BLAKE3 of the deterministic compiled source package.
    pub source_digest: String,
    /// Explicit resource-directory override used by the recording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<PathBuf>,
    /// Root entropy seed and fixed civil time for independent source/ASB runs.
    pub environment: RunEnvironment,
    /// Narrative choices and UI control activations, in execution order.
    pub actions: Vec<ActionSelection>,
    /// Accepted input observation at which this incomplete trace stops.
    pub boundary: Option<ReplayBoundary>,
}

/// A recorded input boundary that remains explicitly incomplete on replay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayBoundary {
    /// Incomplete result observed at this boundary.
    pub outcome: RunOutcome,
    /// Input protocol used by the accepted frame.
    pub kind: ActionKind,
    /// Complete ordered identities offered at the stopping point.
    pub option_keys: Arc<[String]>,
    /// Fingerprint of controls, reading progress, host window and isolated storage.
    pub fingerprint: String,
    /// Original observation or checkpoint limitation.
    pub reason: String,
}

impl ReplayTrace {
    /// Validates the format, package digest and selected target indices.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == SCHEMA_VERSION,
            "unsupported replay schema version {} (expected {SCHEMA_VERSION})",
            self.schema_version
        );
        ensure!(
            self.source_digest.len() == 64
                && self
                    .source_digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit()),
            "replay source digest must be a BLAKE3 hexadecimal string"
        );
        for (index, action) in self.actions.iter().enumerate() {
            ensure!(
                action.selected < action.option_keys.len(),
                "replay action {index} selects an unavailable target"
            );
            ensure!(
                action.kind == ActionKind::Choice || action.input_state.is_some(),
                "replay control action {index} has no accepted input state"
            );
            if let Some(state) = &action.input_state {
                ensure!(
                    state.len() == 64 && state.bytes().all(|byte| byte.is_ascii_hexdigit()),
                    "replay action {index} has an invalid input state digest"
                );
            }
        }
        if let Some(boundary) = &self.boundary {
            ensure!(
                matches!(
                    boundary.outcome,
                    RunOutcome::UiCycle | RunOutcome::CheckpointUnavailable
                ),
                "replay boundary must describe an observed incomplete result"
            );
            ensure!(
                !boundary.option_keys.is_empty(),
                "replay boundary has no input targets"
            );
            ensure!(
                boundary.fingerprint.len() == 64
                    && boundary
                        .fingerprint
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit()),
                "invalid replay boundary fingerprint"
            );
        }
        Ok(())
    }
}

/// Combined result of execution and observed action exploration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    /// Every discovered route completed with matching source and ASB playback.
    Passed,
    /// An execution or source/ASB replay comparison failed.
    Failed,
    /// Discovered routes remain pending or execution needs unsupported input.
    Incomplete,
}

impl TestStatus {
    /// Stable machine-readable status spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Incomplete => "incomplete",
        }
    }
}

/// Source execution and strict playback of its recorded actions from ASB.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunPair {
    /// Source execution with independent persistent and save storage.
    pub source: RunReport,
    /// ASB execution replaying the source's actual actions.
    pub asb: RunReport,
}

/// One node in the shared action-prefix table.
///
/// Node zero is the empty prefix. Other nodes append their selection to the
/// parent's route; parent IDs precede their children.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoutePrefix {
    /// Preceding prefix ID, or None for the empty prefix.
    pub parent: Option<usize>,
    /// Appended action, or None for the empty prefix.
    pub selection: Option<ActionSelection>,
}

/// One attempted route and its actual execution reports.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExplorationRun {
    /// Prefix-table ID requested for this execution.
    pub requested_prefix: usize,
    /// Prefix-table ID of the source's actual actions.
    pub reached_prefix: usize,
    /// Source execution and source-located diagnostics.
    pub source: RunReport,
    /// ASB execution and source-located diagnostics when debug data is present.
    pub asb: RunReport,
    /// Requested-prefix or source/ASB identity mismatch, independent of timing.
    pub replay_mismatch: Option<String>,
    /// Exact source observation at which an incomplete execution stopped.
    pub boundary: Option<ReplayBoundary>,
}

/// Versioned report of observed action routes in the current game.
///
/// This records narrative selections and accepted UI targets. It makes no claim
/// about total business-state or ordinary conditional-branch coverage.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExplorationReport {
    /// Report format, currently SCHEMA_VERSION.
    pub schema_version: u32,
    /// Aggregate result; actual failures take priority over incomplete coverage.
    pub status: TestStatus,
    /// Attempted routes in execution order.
    pub runs: Vec<ExplorationRun>,
    /// Shared prefix table referenced by routes and coverage lists.
    pub prefixes: Vec<RoutePrefix>,
    /// Distinct actual source routes observed by run attempts.
    pub reached: Vec<usize>,
    /// Prefixes for which an execution was attempted.
    pub explored: Vec<usize>,
    /// Discovered routes whose execution remains unresolved.
    pub pending: Vec<usize>,
}

impl ExplorationReport {
    /// Reconstructs one exact action route from the shared prefix table.
    pub fn prefix(&self, id: usize) -> Result<Vec<ActionSelection>> {
        let mut actions = Vec::new();
        let mut current = id;
        loop {
            let node = self
                .prefixes
                .get(current)
                .context("invalid route prefix ID")?;
            match (&node.parent, &node.selection) {
                (None, None) if current == 0 => break,
                (Some(parent), Some(selection)) if *parent < current => {
                    actions.push(selection.clone());
                    current = *parent;
                }
                _ => anyhow::bail!("invalid route prefix parent or selection"),
            }
        }
        actions.reverse();
        Ok(actions)
    }
}

/// Records checkpoint-backed exploration without retaining execution snapshots.
pub struct ExplorationRecorder {
    report: ExplorationReport,
    children: Vec<Vec<usize>>,
}

impl Default for ExplorationRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl ExplorationRecorder {
    /// Starts a game report with its empty entry route pending.
    pub fn new() -> Self {
        Self {
            report: ExplorationReport {
                schema_version: SCHEMA_VERSION,
                status: TestStatus::Incomplete,
                runs: Vec::new(),
                prefixes: vec![RoutePrefix {
                    parent: None,
                    selection: None,
                }],
                reached: Vec::new(),
                explored: Vec::new(),
                pending: vec![0],
            },
            children: vec![Vec::new()],
        }
    }

    /// Records an unresolved child route and returns its shared prefix ID.
    pub fn branch(&mut self, parent: usize, selection: ActionSelection) -> Result<usize> {
        ensure!(parent < self.children.len(), "invalid route prefix ID");
        let id = self.insert(parent, selection);
        push_unique(&mut self.report.pending, id);
        Ok(id)
    }

    /// Reconstructs a recorded prefix for playback or checkpoint bookkeeping.
    pub fn prefix(&self, id: usize) -> Result<Vec<ActionSelection>> {
        self.report.prefix(id)
    }

    /// Records one actual source/ASB route and returns its reached prefix ID.
    pub fn record(&mut self, requested_id: usize, pair: RunPair) -> Result<usize> {
        self.record_boundary(requested_id, pair, None)
    }

    /// Records an exact incomplete boundary together with its source/ASB results.
    pub fn record_boundary(
        &mut self,
        requested_id: usize,
        pair: RunPair,
        boundary: Option<ReplayBoundary>,
    ) -> Result<usize> {
        let requested = self.report.prefix(requested_id)?;
        let mut mismatch = compare_replay(&pair);
        for (index, (expected, actual)) in
            requested.iter().zip(pair.source.actions.iter()).enumerate()
        {
            if expected != actual {
                mismatch = Some(format!(
                    "source diverged from requested prefix at action {index}"
                ));
                break;
            }
        }
        if matches!(pair.source.outcome, RunOutcome::Finished)
            && pair.source.actions.len() < requested.len()
        {
            mismatch = Some("source finished before consuming the requested prefix".into());
        }
        let mut reached = 0;
        self.report.pending.retain(|id| *id != 0);
        for (index, selection) in pair.source.actions.iter().enumerate() {
            if selection.selected >= selection.option_keys.len() {
                mismatch = Some(format!(
                    "source recorded an unavailable target at action {index}"
                ));
            }
            reached = self.insert(reached, selection.clone());
            self.report.pending.retain(|id| *id != reached);
        }
        push_unique(&mut self.report.reached, reached);
        push_unique(&mut self.report.explored, requested_id);
        if incomplete(&pair.source.outcome) || incomplete(&pair.asb.outcome) {
            push_unique(&mut self.report.pending, reached);
        }
        self.report.runs.push(ExplorationRun {
            requested_prefix: requested_id,
            reached_prefix: reached,
            source: pair.source,
            asb: pair.asb,
            replay_mismatch: mismatch,
            boundary,
        });
        Ok(reached)
    }

    /// Finalizes observed route coverage and actual execution status.
    pub fn finish(mut self) -> ExplorationReport {
        self.report.status = if self.report.runs.iter().any(|run| {
            run.replay_mismatch.is_some() || failed(&run.source.outcome) || failed(&run.asb.outcome)
        }) {
            TestStatus::Failed
        } else if !self.report.pending.is_empty() {
            TestStatus::Incomplete
        } else {
            TestStatus::Passed
        };
        self.report
    }

    fn insert(&mut self, parent: usize, selection: ActionSelection) -> usize {
        if let Some(id) = self.children[parent]
            .iter()
            .copied()
            .find(|id| self.report.prefixes[*id].selection.as_ref() == Some(&selection))
        {
            return id;
        }
        let id = self.report.prefixes.len();
        self.report.prefixes.push(RoutePrefix {
            parent: Some(parent),
            selection: Some(selection),
        });
        self.children.push(Vec::new());
        self.children[parent].push(id);
        id
    }
}

/// Reports one exact strict playback without discovering alternative routes.
pub fn replay(prefix: &[ActionSelection], pair: RunPair) -> ExplorationReport {
    let mut recorder = ExplorationRecorder::new();
    let mut requested = 0;
    for selection in prefix {
        requested = recorder
            .branch(requested, selection.clone())
            .expect("internal prefix");
    }
    let extra = pair.source.actions.len() > prefix.len();
    recorder.record(requested, pair).expect("internal prefix");
    if extra && recorder.report.runs[0].replay_mismatch.is_none() {
        recorder.report.runs[0].replay_mismatch =
            Some("source exceeded the complete recorded route".into());
    }
    recorder.finish()
}

/// Compares action identities and terminal results, excluding wall timing.
pub fn compare_replay(pair: &RunPair) -> Option<String> {
    for (index, (source, asb)) in pair
        .source
        .actions
        .iter()
        .zip(pair.asb.actions.iter())
        .enumerate()
    {
        if source.kind != asb.kind {
            return Some(format!("source/ASB action kinds differ at action {index}"));
        }
        if source.option_keys != asb.option_keys {
            return Some(format!(
                "source/ASB offered target identities differ at action {index}"
            ));
        }
        if source.input_state != asb.input_state {
            return Some(format!(
                "source/ASB editable input states differ at action {index}"
            ));
        }
        if source.selected != asb.selected {
            return Some(format!(
                "source/ASB selected target differs at action {index}"
            ));
        }
    }
    let checkpoint_interrupted = matches!(pair.source.outcome, RunOutcome::CheckpointUnavailable)
        || matches!(pair.asb.outcome, RunOutcome::CheckpointUnavailable);
    if pair.source.actions.len() != pair.asb.actions.len() && !checkpoint_interrupted {
        return Some(format!(
            "source/ASB action count differs ({} versus {})",
            pair.source.actions.len(),
            pair.asb.actions.len()
        ));
    }
    if pair.source.outcome != pair.asb.outcome && !checkpoint_interrupted {
        return Some(format!(
            "source/ASB terminal outcomes differ ({:?} versus {:?})",
            pair.source.outcome, pair.asb.outcome
        ));
    }
    None
}

fn push_unique(values: &mut Vec<usize>, id: usize) {
    if !values.contains(&id) {
        values.push(id);
    }
}

fn incomplete(outcome: &RunOutcome) -> bool {
    !matches!(outcome, RunOutcome::Finished) && !failed(outcome)
}

fn failed(outcome: &RunOutcome) -> bool {
    matches!(
        outcome,
        RunOutcome::ReplayMismatch | RunOutcome::RuntimeFailure | RunOutcome::ResourceFailure
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::{ActionKind, RunDiagnostic, SourceFrame};
    use std::time::Duration;

    fn action(kind: ActionKind, selected: usize) -> ActionSelection {
        ActionSelection {
            kind,
            option_keys: vec!["left".into(), "right".into()].into(),
            input_state: (kind == ActionKind::Controls).then(|| "cd".repeat(32).into()),
            selected,
        }
    }

    fn choice(selected: usize) -> ActionSelection {
        action(ActionKind::Choice, selected)
    }

    fn pair(actions: Vec<ActionSelection>, outcome: RunOutcome) -> RunPair {
        let source = RunReport {
            outcome,
            steps: actions.len() as u64 + 1,
            actions: actions.into(),
            elapsed: Duration::ZERO,
            diagnostic: None,
            accepted_frames: 0,
            audio_frames: 0,
            audio_peak: 0.0,
        };
        RunPair {
            asb: source.clone(),
            source,
        }
    }

    #[test]
    fn combinatorial_routes_share_parent_prefixes_and_clear_pending_routes() -> Result<()> {
        let mut recorder = ExplorationRecorder::new();
        for route in 0..8 {
            let actions = (0..3)
                .map(|shift| choice((route >> shift) & 1))
                .collect::<Vec<_>>();
            let mut requested = 0;
            for selected in &actions {
                requested = recorder.branch(requested, selected.clone())?;
            }
            recorder.record(requested, pair(actions, RunOutcome::Finished))?;
        }
        let report = recorder.finish();
        assert_eq!(report.status, TestStatus::Passed);
        assert_eq!(report.runs.len(), 8);
        assert_eq!(report.reached.len(), 8);
        assert_eq!(report.prefixes.len(), 15);
        assert!(report.pending.is_empty());
        Ok(())
    }

    #[test]
    fn unexplored_ui_control_is_retained_as_an_exact_pending_route() -> Result<()> {
        let mut recorder = ExplorationRecorder::new();
        let controls = action(ActionKind::Controls, 1);
        let pending = recorder.branch(0, controls.clone())?;
        recorder.record(
            0,
            pair(vec![action(ActionKind::Controls, 0)], RunOutcome::Finished),
        )?;
        let report = recorder.finish();
        assert_eq!(report.status, TestStatus::Incomplete);
        assert_eq!(report.pending, vec![pending]);
        assert_eq!(report.prefix(pending)?, vec![controls]);
        Ok(())
    }

    #[test]
    fn source_asb_target_kind_and_terminal_mismatches_fail() {
        let mut changed_options = pair(vec![choice(0)], RunOutcome::Finished);
        let mut changed = choice(0);
        Arc::make_mut(&mut changed.option_keys).swap(0, 1);
        changed_options.asb.actions = vec![changed].into();
        assert_eq!(
            replay(&[choice(0)], changed_options).status,
            TestStatus::Failed
        );
        let mut changed_kind = pair(vec![choice(0)], RunOutcome::Finished);
        changed_kind.asb.actions = vec![action(ActionKind::Controls, 0)].into();
        assert_eq!(
            replay(&[choice(0)], changed_kind).status,
            TestStatus::Failed
        );
        let mut changed_outcome = pair(Vec::new(), RunOutcome::Finished);
        changed_outcome.asb.outcome = RunOutcome::RuntimeFailure;
        assert_eq!(replay(&[], changed_outcome).status, TestStatus::Failed);
    }

    #[test]
    fn exact_replay_ignores_elapsed_time_and_rejects_unused_actions() {
        let prefix = vec![action(ActionKind::Controls, 1), choice(0)];
        let mut execution = pair(prefix.clone(), RunOutcome::Finished);
        execution.source.elapsed = Duration::from_millis(3);
        execution.asb.elapsed = Duration::from_secs(42);
        let report = replay(&prefix, execution);
        assert_eq!(report.status, TestStatus::Passed);
        assert!(report.pending.is_empty());
        assert_eq!(
            report.prefix(report.runs[0].reached_prefix).unwrap(),
            prefix
        );
        assert_eq!(
            replay(&[choice(0)], pair(Vec::new(), RunOutcome::Finished)).status,
            TestStatus::Failed
        );
    }

    #[test]
    fn runtime_failure_retains_the_exact_route_and_source_location() {
        let mut execution = pair(vec![choice(1)], RunOutcome::RuntimeFailure);
        execution.source.diagnostic = Some(RunDiagnostic {
            message: "assertion failed".into(),
            stack: vec![SourceFrame {
                module: "game::route".into(),
                function: "ending".into(),
                path: Some("src/route.aside".into()),
                line: Some(12),
                column: Some(8),
                pc: 5,
            }],
        });
        let report = replay(&[choice(1)], execution);
        assert_eq!(report.status, TestStatus::Failed);
        assert_eq!(
            report.prefix(report.runs[0].reached_prefix).unwrap(),
            vec![choice(1)]
        );
        let frame = &report.runs[0].source.diagnostic.as_ref().unwrap().stack[0];
        assert_eq!(frame.path.as_deref(), Some("src/route.aside"));
        assert_eq!(frame.line, Some(12));
    }

    #[test]
    fn playback_json_round_trip_preserves_control_and_choice_identity() -> Result<()> {
        let trace = ReplayTrace {
            schema_version: SCHEMA_VERSION,
            source_digest: "ab".repeat(32),
            assets: Some(PathBuf::from("test-assets")),
            environment: RunEnvironment {
                seed: 17,
                unix_seconds: 1_700_000_000,
            },
            actions: vec![action(ActionKind::Controls, 1), choice(0)],
            boundary: None,
        };
        trace.validate()?;
        let decoded: ReplayTrace = serde_json::from_slice(&serde_json::to_vec(&trace)?)?;
        assert_eq!(trace, decoded);
        let mut invalid = decoded;
        invalid.actions[0].selected = 2;
        assert!(invalid.validate().is_err());
        Ok(())
    }

    #[test]
    fn checkpoint_interruption_keeps_pending_routes_and_prior_game_faults() -> Result<()> {
        let mut recorder = ExplorationRecorder::new();
        let failing = recorder.branch(0, choice(0))?;
        let pending = recorder.branch(0, choice(1))?;
        let mut fault = pair(vec![choice(0)], RunOutcome::RuntimeFailure);
        fault.source.diagnostic = Some(RunDiagnostic {
            message: "original game fault".into(),
            stack: Vec::new(),
        });
        recorder.record(failing, fault)?;
        recorder.record(pending, pair(Vec::new(), RunOutcome::CheckpointUnavailable))?;
        let report = recorder.finish();
        assert_eq!(report.status, TestStatus::Failed);
        assert!(report.pending.contains(&pending));
        assert_eq!(
            report.runs[0].source.diagnostic.as_ref().unwrap().message,
            "original game fault"
        );

        let source = pair(vec![choice(0)], RunOutcome::Finished).source;
        let asb = pair(Vec::new(), RunOutcome::CheckpointUnavailable).source;
        let report = replay(&[choice(0)], RunPair { source, asb });
        assert_eq!(report.status, TestStatus::Incomplete);
        assert!(report.runs[0].replay_mismatch.is_none());
        Ok(())
    }
}
