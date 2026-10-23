//! v1 headless story execution through the production session and CPU presentation.
//!
//! Each invocation owns fresh save storage and media, advances dialogue using
//! ordinary input, and releases each compiled CPU frame and offline PCM block.
//! Only a naturally completed story (including an authored exit) passes. Mounted
//! interactive pages are explored through their accepted input geometry.
mod audio;
mod explore;
mod presentation;
pub use explore::{explore, explore_with_environment};
#[cfg(test)]
mod tests;

use aside_vm::{bytecode::BytecodePackage, executor::Location};
use reel_core::{budget::QueueLimits, wake::WakeSignal};
use reel_engine::{
    Epoch, InputAction, InteractionView, Session, SessionId,
    aside::{SliceBudget, VmLimits, source::CompiledStory},
};
use reel_player::{
    aside_session::{AsideSessionUi, StoryState},
    assets::AssetOptions,
    saves::{DirectorySaves, RuntimeSaves, SaveService},
    story_runtime::StoryRuntime,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    collections::VecDeque,
    path::Path,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

/// Playback policy. VM and native work retain their ordinary per-slice budgets.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RunOptions {
    /// Require the supplied choice trace to match the complete execution.
    pub strict_replay: bool,
    /// Entropy and civil time used by this execution and its playback.
    pub environment: RunEnvironment,
}

/// Reproducible process inputs shared by source execution and ASB playback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEnvironment {
    /// Root seed used to allocate independent unseeded random streams.
    pub seed: u64,
    /// Fixed UTC Unix second supplied throughout this check.
    pub unix_seconds: i64,
}

impl RunEnvironment {
    /// Capture civil time once; use the v1 root seed for reproducible exploration.
    pub fn capture() -> Self {
        Self {
            seed: 0x5245_454c_0000_0001,
            unix_seconds: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or_else(
                    |error| -(error.duration().as_secs() as i64),
                    |time| time.as_secs() as i64,
                ),
        }
    }
}

impl Default for RunEnvironment {
    fn default() -> Self {
        Self::capture()
    }
}

struct DeterministicEnvironment {
    state: Cell<u64>,
    unix_seconds: i64,
}

impl DeterministicEnvironment {
    fn state(&self) -> u64 {
        self.state.get()
    }
    fn set_state(&self, state: u64) {
        self.state.set(state);
    }
}

impl reel_engine::aside::HostEnvironment for DeterministicEnvironment {
    fn random_seed(&self) -> u64 {
        let mut generator = fastrand::Rng::with_seed(self.state.get());
        let seed = generator.u64(..);
        self.state.set(generator.get_seed());
        seed
    }
    fn unix_seconds(&self) -> i64 {
        self.unix_seconds
    }
}

/// Kind of interaction submitted through the ordinary host input path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Choice,
    Controls,
}

/// Ordered business keys and the selected zero-based option at one choice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionSelection {
    pub kind: ActionKind,
    pub option_keys: Vec<String>,
    pub selected: usize,
}

/// Terminal classification of one execution. Only `Finished` is a passing run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Finished,
    ReplayMismatch,
    RuntimeFailure,
    ResourceFailure,
    /// The live story is waiting for input that this host cannot discover.
    Blocked,
    /// The same observed control state and action have already been visited.
    UiCycle,
    /// A temporary checkpoint could not be captured or restored by this host.
    CheckpointUnavailable,
}

/// An immutable source location copied from the actual VM fault stack.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceFrame {
    pub module: String,
    pub function: String,
    pub path: Option<String>,
    /// One-based source line when bytecode contains source metadata.
    pub line: Option<u32>,
    /// One-based Unicode scalar column when source metadata is available.
    pub column: Option<u32>,
    pub pc: u32,
}

/// Failure details retained after the owning session has been released.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunDiagnostic {
    pub message: String,
    pub stack: Vec<SourceFrame>,
}

/// Summary of one execution; frames and PCM are consumed immediately.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunReport {
    pub outcome: RunOutcome,
    pub actions: Vec<ActionSelection>,
    pub steps: u64,
    pub elapsed: Duration,
    pub diagnostic: Option<RunDiagnostic>,
    pub accepted_frames: u64,
    pub audio_frames: u64,
    pub audio_peak: f32,
}

fn source_frame(package: &BytecodePackage, location: Location) -> SourceFrame {
    let function = &package.functions[location.code.function.index()];
    let module = &package.modules[function.module.index()];
    let mut frame = SourceFrame {
        module: package.symbols[module.path.index()].clone(),
        function: package.symbols[function.name.index()].clone(),
        path: None,
        line: None,
        column: None,
        pc: location.pc.0,
    };
    if let Some(span) = location.source {
        let source = &package.sources[span.file.index()];
        let before = &source.text[..span.start as usize];
        frame.path = Some(package.symbols[source.path.index()].clone());
        frame.line = Some(before.bytes().filter(|byte| *byte == b'\n').count() as u32 + 1);
        frame.column = Some(before.rsplit('\n').next().unwrap_or("").chars().count() as u32 + 1);
    }
    frame
}

fn diagnostic(runtime: &StoryRuntime, message: String) -> RunDiagnostic {
    let mut diagnostic = RunDiagnostic {
        message,
        stack: Vec::new(),
    };
    let vm = runtime.driver().adapter().vm();
    let Ok(Some(fault)) = runtime.driver().fault() else {
        return diagnostic;
    };
    if let Ok(mut stack) = vm.heap().fault_stack(fault.id) {
        while let Ok(Some(frame)) = stack.next(vm.heap()) {
            diagnostic
                .stack
                .push(source_frame(vm.program().package(), frame.location));
        }
    }
    if diagnostic.stack.is_empty() {
        diagnostic
            .stack
            .push(source_frame(vm.program().package(), fault.location));
    }
    diagnostic
}

type DriveFailure = (RunOutcome, RunDiagnostic);

struct Decision {
    kind: ActionKind,
    option_keys: Vec<String>,
    actions: Vec<DecisionAction>,
}

enum DecisionAction {
    Choice(InputAction),
    Control {
        position: [f64; 2],
        text: Option<&'static str>,
    },
}

enum PhysicalInput {
    Pointer(reel_ui::core::PointerEvent),
    Text(reel_ui::core::TextEvent),
}

struct Runner {
    runtime: StoryRuntime,
    presentation: presentation::Presentation,
    audio: audio::AudioClock,
    saves: tempfile::TempDir,
    wake: WakeSignal,
    host: Rc<DeterministicEnvironment>,
    steps: u64,
    logical_ns: u64,
    started: Instant,
    actions: Vec<ActionSelection>,
    pending_input: VecDeque<PhysicalInput>,
}

impl Runner {
    fn new_with_environment(
        story: CompiledStory,
        resources: &Path,
        environment: &RunEnvironment,
    ) -> anyhow::Result<Self> {
        let saves = tempfile::tempdir()?;
        let thread = std::thread::current();
        let wake = WakeSignal::new(move || thread.unpark());
        let audio = audio::AudioClock::default();
        let media = audio.factory(resources.to_owned(), wake.clone());
        let host = Rc::new(DeterministicEnvironment {
            state: Cell::new(environment.seed),
            unix_seconds: environment.unix_seconds,
        });
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1))
                .map_err(|error| anyhow::anyhow!("session: {error:?}"))?,
            story,
            "en",
            VmLimits::default(),
        )
        .map_err(|error| anyhow::anyhow!("story setup: {error}"))?
        .with_asset_options(AssetOptions {
            root: resources.to_owned(),
            ..Default::default()
        })
        .with_host_environment(host.clone())
        .with_media(media())
        .map_err(|error| anyhow::anyhow!("media setup: {error}"))?;
        let resource_wake = wake.clone();
        driver.set_resource_waker(Arc::new(move || resource_wake.wake()));
        let storage = SaveService::new(
            Arc::new(DirectorySaves::new(saves.path(), 256 << 20)),
            QueueLimits {
                max_requests: 16,
                max_bytes: 256 << 20,
            },
            wake.clone(),
        );
        let runtime = StoryRuntime::new(driver)
            .with_saves(RuntimeSaves::new(storage, 256 << 20))
            .with_media_factory(media);
        let presentation = presentation::Presentation::new(runtime.driver())
            .map_err(|error| anyhow::anyhow!(error))?;
        Ok(Self {
            runtime,
            presentation,
            audio,
            saves,
            wake,
            host,
            steps: 0,
            logical_ns: 0,
            started: Instant::now(),
            actions: Vec::new(),
            pending_input: VecDeque::new(),
        })
    }

    fn failure(&self, outcome: RunOutcome, message: impl Into<String>) -> DriveFailure {
        (outcome, diagnostic(&self.runtime, message.into()))
    }

    fn reset_presentation(&mut self) -> Result<(), DriveFailure> {
        let frames = self.presentation.frames;
        self.presentation = presentation::Presentation::new(self.runtime.driver())
            .map_err(|error| self.failure(RunOutcome::ResourceFailure, error))?;
        self.presentation.frames = frames;
        self.pending_input.clear();
        Ok(())
    }

    /// Run ordinary VM slices at the current virtual instant. Advance time only
    /// when the scheduler reaches a timed wait, and park only for real asset IO.
    fn drive(&mut self) -> Result<Option<Decision>, DriveFailure> {
        loop {
            if !self.runtime.capturing() && self.runtime.driver().adapter().has_capture_requests() {
                let pending = self
                    .runtime
                    .frame_capture_pending()
                    .map_err(|error| self.failure(RunOutcome::RuntimeFailure, error.to_string()))?;
                if pending {
                    return Err(self.failure(
                        RunOutcome::Blocked,
                        "stage.capture requires a rendering backend; this CPU check stopped at the capture request",
                    ));
                }
            }
            if self.wake.acknowledge() {
                self.runtime.wake_input();
            }
            self.steps += 1;
            self.audio
                .advance(Duration::from_nanos(self.logical_ns))
                .map_err(|error| self.failure(RunOutcome::ResourceFailure, error))?;
            let identity = self.runtime.driver().ui().session().identity();
            self.runtime
                .tick(SliceBudget::default(), || self.logical_ns)
                .map_err(|error| self.failure(RunOutcome::RuntimeFailure, error.to_string()))?;
            if let Some(blocked) = self.runtime.resource_blocked() {
                return Err(self.failure(
                    RunOutcome::ResourceFailure,
                    format!("VM resource blocked after GC: {blocked}"),
                ));
            }
            if self.runtime.close_complete() {
                return Ok(None);
            }
            if identity != self.runtime.driver().ui().session().identity() {
                self.reset_presentation()?;
            }
            let mut submitted_input = false;
            if !self.runtime.capturing() {
                let idle = !self.runtime.needs_tick();
                let checkpoint_ready = self.runtime.checkpoint_ready();
                let driver = self.runtime.driver_mut().map_err(|error| {
                    (
                        RunOutcome::RuntimeFailure,
                        RunDiagnostic {
                            message: error.to_string(),
                            stack: Vec::new(),
                        },
                    )
                })?;
                let presented = self
                    .presentation
                    .present(driver, self.steps)
                    .map_err(|error| {
                        (
                            RunOutcome::ResourceFailure,
                            RunDiagnostic {
                                message: error,
                                stack: Vec::new(),
                            },
                        )
                    })?;
                if presented {
                    if driver.state() == StoryState::Finished && idle {
                        return Ok(None);
                    }
                    if let Some(input) = self.pending_input.pop_front() {
                        match input {
                            PhysicalInput::Pointer(event) => {
                                driver.handle_pointer(&mut self.presentation.bridge, event)
                            }
                            PhysicalInput::Text(event) => {
                                driver.handle_text(&mut self.presentation.bridge, event)
                            }
                        }
                        .map_err(|error| {
                            (
                                RunOutcome::RuntimeFailure,
                                RunDiagnostic {
                                    message: error.to_string(),
                                    stack: Vec::new(),
                                },
                            )
                        })?;
                        submitted_input = true;
                    } else if checkpoint_ready
                        && !driver.has_runnable_tasks()
                        && !driver.activities_pending()
                    {
                        if driver.narrative_input_ready() {
                            match driver.ui().session().view().interaction {
                                Some(InteractionView::Choice(choice)) => {
                                    return Ok(Some(Decision {
                                        kind: ActionKind::Choice,
                                        option_keys: choice
                                            .options()
                                            .iter()
                                            .map(|option| option.key().to_owned())
                                            .collect(),
                                        actions: choice
                                            .options()
                                            .iter()
                                            .map(|option| {
                                                DecisionAction::Choice(InputAction::Select(
                                                    option.id().clone(),
                                                ))
                                            })
                                            .collect(),
                                    }));
                                }
                                Some(InteractionView::Dialogue(dialogue))
                                    if dialogue.reveal_skippable()
                                        || dialogue.awaiting_advance() =>
                                {
                                    let action = InputAction::Advance(dialogue.id());
                                    submitted_input = driver
                                        .handle_input(action)
                                        .map_err(|error| {
                                            (
                                                RunOutcome::RuntimeFailure,
                                                RunDiagnostic {
                                                    message: error.to_string(),
                                                    stack: Vec::new(),
                                                },
                                            )
                                        })?
                                        .is_some();
                                }
                                _ => {}
                            }
                        }
                        if !submitted_input
                            && (!driver.narrative_input_ready()
                                || driver.ui().session().view().interaction.is_none())
                        {
                            let targets = driver
                                .input_targets(&self.presentation.bridge, self.presentation.root);
                            if !targets.is_empty() {
                                return Ok(Some(Decision {
                                    kind: ActionKind::Controls,
                                    option_keys: targets
                                        .iter()
                                        .map(|target| target.key.clone())
                                        .collect(),
                                    actions: targets
                                        .into_iter()
                                        .map(|target| DecisionAction::Control {
                                            position: target.position,
                                            text: target.accepts_text.then_some("Player"),
                                        })
                                        .collect(),
                                }));
                            }
                        }
                    }
                }
            }
            if submitted_input || self.runtime.driver().has_runnable_tasks() {
                self.runtime.wake_input();
                continue;
            }
            if self.runtime.driver().activities_pending() || self.runtime.driver().cleanup_pending()
            {
                continue;
            }
            if self.runtime.driver().state() == StoryState::Finished
                && !self.runtime.needs_tick()
                && !self.runtime.driver().ui().images_pending()
                && self.presentation.fonts_ready()
            {
                return Ok(None);
            }
            if self.runtime.driver().state() == StoryState::Cancelled {
                self.close()?;
                return Ok(None);
            }
            let driver = self.runtime.driver();
            let assets = driver.ui().assets();
            if (driver.ui().images_pending() || !self.presentation.fonts_ready())
                && assets.pending_io()
            {
                if !self.wake.is_pending() {
                    std::thread::park();
                }
                continue;
            }
            if let Some(deadline) = self
                .runtime
                .wake_deadline(reel_core::clock::MonoTime(self.logical_ns))
            {
                self.logical_ns = self.logical_ns.max(deadline.0);
                // Decode and save workers share CPU with accelerated execution.
                std::thread::yield_now();
            } else {
                return Err(self.failure(RunOutcome::Blocked, format!("story is waiting without an actionable accepted control or a scheduled wake; state={:?}, tasks={}, activities={}, images={}",driver.state(),driver.has_runnable_tasks(),driver.activities_pending(),driver.ui().images_pending())));
            }
        }
    }

    fn apply(&mut self, decision: &Decision, selected: usize) -> Result<(), DriveFailure> {
        let Some(action) = decision.actions.get(selected) else {
            return Err(self.failure(RunOutcome::ReplayMismatch, "selected action is unavailable"));
        };
        match action {
            DecisionAction::Choice(action) => {
                let outcome = self
                    .runtime
                    .driver_mut()
                    .map_err(|error| {
                        (
                            RunOutcome::RuntimeFailure,
                            RunDiagnostic {
                                message: error.to_string(),
                                stack: Vec::new(),
                            },
                        )
                    })?
                    .handle_input(action.clone())
                    .map_err(|error| self.failure(RunOutcome::RuntimeFailure, error.to_string()))?;
                if outcome.is_none() {
                    return Err(self.failure(
                        RunOutcome::ReplayMismatch,
                        "the selected narrative action no longer has input permission",
                    ));
                }
            }
            DecisionAction::Control {
                position: [x, y],
                text,
            } => {
                use reel_ui::{core::*, dpi::PhysicalPosition};
                let pointer = PointerInfo {
                    pointer_id: Some(PointerId::PRIMARY),
                    persistent_device_id: None,
                    pointer_type: PointerType::Mouse,
                };
                let state = PointerState {
                    position: PhysicalPosition::new(*x, *y),
                    ..Default::default()
                };
                self.pending_input.extend([
                    PhysicalInput::Pointer(PointerEvent::Move(PointerUpdate {
                        pointer,
                        current: state.clone(),
                        coalesced: vec![],
                        predicted: vec![],
                    })),
                    PhysicalInput::Pointer(PointerEvent::Down(PointerButtonEvent {
                        pointer,
                        state: state.clone(),
                        button: Some(PointerButton::Primary),
                    })),
                    PhysicalInput::Pointer(PointerEvent::Up(PointerButtonEvent {
                        pointer,
                        state,
                        button: Some(PointerButton::Primary),
                    })),
                ]);
                if let Some(text) = text {
                    self.pending_input
                        .push_back(PhysicalInput::Text(TextEvent::ClipboardPaste(
                            (*text).into(),
                        )));
                }
            }
        }
        self.actions.push(ActionSelection {
            kind: decision.kind,
            option_keys: decision.option_keys.clone(),
            selected,
        });
        self.runtime.wake_input();
        Ok(())
    }

    fn close(&mut self) -> Result<(), DriveFailure> {
        self.runtime
            .close()
            .map_err(|error| self.failure(RunOutcome::RuntimeFailure, error.to_string()))?;
        while !self.runtime.close_complete() {
            self.runtime
                .tick(SliceBudget::default(), || self.logical_ns)
                .map_err(|error| self.failure(RunOutcome::RuntimeFailure, error.to_string()))?;
            std::thread::yield_now();
        }
        Ok(())
    }

    fn report(&self, outcome: RunOutcome, diagnostic: Option<RunDiagnostic>) -> RunReport {
        RunReport {
            outcome,
            actions: self.actions.clone(),
            steps: self.steps,
            elapsed: self.started.elapsed(),
            diagnostic,
            accepted_frames: self.presentation.frames,
            audio_frames: self.audio.frames,
            audio_peak: self.audio.peak,
        }
    }
}

/// Run a fresh story using real assets and accepted CPU frames on a virtual clock.
/// The supplied action prefix is validated against each decision. Strict replay
/// requires the complete trace; ordinary execution selects the first remaining
/// action. Each run owns isolated save and persistent storage.
pub fn run(
    story: CompiledStory,
    resources: &Path,
    options: &RunOptions,
    prefix: &[ActionSelection],
) -> anyhow::Result<RunReport> {
    let mut runner = Runner::new_with_environment(story, resources, &options.environment)?;
    let result = loop {
        let decision = match runner.drive() {
            Ok(Some(decision)) => decision,
            Ok(None) => {
                if runner.actions.len() < prefix.len() {
                    break Err(runner.failure(
                        RunOutcome::ReplayMismatch,
                        "story ended with unused replay actions",
                    ));
                }
                break Ok(());
            }
            Err(error) => break Err(error),
        };
        let selected = if let Some(expected) = prefix.get(runner.actions.len()) {
            if expected.kind != decision.kind
                || expected.option_keys != decision.option_keys
                || expected.selected >= decision.actions.len()
            {
                break Err(runner.failure(
                    RunOutcome::ReplayMismatch,
                    format!(
                        "action {} differs from replay: expected {expected:?}, offered {:?}",
                        runner.actions.len() + 1,
                        decision.option_keys
                    ),
                ));
            }
            expected.selected
        } else if options.strict_replay {
            break Err(runner.failure(
                RunOutcome::ReplayMismatch,
                format!(
                    "unexpected action {}: {:?}",
                    runner.actions.len() + 1,
                    decision.option_keys
                ),
            ));
        } else {
            0
        };
        if let Err(error) = runner.apply(&decision, selected) {
            break Err(error);
        }
    };
    let mut report = match result {
        Ok(()) => runner.report(RunOutcome::Finished, None),
        Err((outcome, diagnostic)) => runner.report(outcome, Some(diagnostic)),
    };
    if let Err((outcome, diagnostic)) = runner.close()
        && report.outcome == RunOutcome::Finished
    {
        report.outcome = outcome;
        report.diagnostic = Some(diagnostic);
    }
    Ok(report)
}
