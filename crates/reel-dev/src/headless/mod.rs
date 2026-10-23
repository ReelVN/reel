//! v1 headless story execution through the production session and CPU presentation.
//!
//! Each invocation owns fresh save storage and media, advances dialogue using
//! ordinary input, and releases each compiled CPU frame and offline PCM block.
//! Only a naturally completed story (including an authored exit) passes. Mounted
//! interactive pages are explored through their accepted input geometry.
mod audio;
mod capture;
mod explore;
mod presentation;
mod trace;
mod window;
pub use explore::{explore, explore_with_environment, explore_with_observer, run_to_boundary};
pub use trace::ActionTrace;
#[cfg(test)]
mod boundary_tests;
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

/// Kind of narrative or accepted control boundary selected by the check host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Choice,
    /// Accepted controls, including an optional reserved finite-wait option.
    Controls,
}

/// Ordered target keys and the selected zero-based option at one boundary.
/// The reserved `@wait:finite-activities` control option advances only the finite
/// activity waits present at that observation, without submitting author input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionSelection {
    pub kind: ActionKind,
    /// Shared ordered identities; serialized as an ordinary string array.
    pub option_keys: Arc<[String]>,
    /// Digest of accepted editable values; narrative choices use null.
    pub input_state: Option<Arc<str>>,
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
    pub actions: ActionTrace,
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
    option_keys: Arc<[String]>,
    input_state: Option<Arc<str>>,
    actions: Vec<DecisionAction>,
}

enum DecisionAction {
    Choice(InputAction),
    Control {
        position: [f64; 2],
        text: Option<&'static str>,
        submit: bool,
        binding: Option<(reel_ui::input::Binding, f32)>,
    },
    WaitForActivities(Vec<aside_vm::executor::WaitId>),
}

const WAIT_FINITE: &str = "@wait:finite-activities";

fn control_input_state(targets: &[reel_player::session::InputTarget]) -> Arc<str> {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let mut hash = Sha256::new();
    for target in targets {
        for value in [&target.key, &target.state] {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value.as_bytes());
        }
    }
    let mut encoded = String::with_capacity(64);
    for byte in hash.finalize() {
        write!(encoded, "{byte:02x}").expect("formatting into String");
    }
    encoded.into()
}

enum PhysicalInput {
    Pointer(reel_ui::core::PointerEvent),
    Text(reel_ui::core::TextEvent),
}

/// A deterministic virtual keyboard uses the ordinary desktop event path.
/// Physical codes remain authoritative for bindings; character keys use US layout.
fn keyboard_event(
    code: reel_ui::core::keyboard::Code,
    state: reel_ui::core::keyboard::KeyState,
) -> reel_ui::core::keyboard::KeyboardEvent {
    use reel_ui::core::keyboard::*;
    let name = code.to_string();
    let logical = match name.as_str() {
        "Space" => " ",
        "NumpadEnter" => "Enter",
        "ControlLeft" | "ControlRight" => "Control",
        "ShiftLeft" | "ShiftRight" => "Shift",
        "AltLeft" | "AltRight" => "Alt",
        "MetaLeft" | "MetaRight" => "Meta",
        "Backquote" => "`",
        "Backslash" | "IntlBackslash" => "\\",
        "BracketLeft" => "[",
        "BracketRight" => "]",
        "Comma" => ",",
        "Equal" => "=",
        "Minus" | "NumpadSubtract" => "-",
        "Period" | "NumpadDecimal" => ".",
        "Quote" => "'",
        "Semicolon" => ";",
        "Slash" | "NumpadDivide" => "/",
        "NumpadAdd" => "+",
        "NumpadMultiply" => "*",
        "NumpadEqual" => "=",
        other => other.strip_prefix("Digit").unwrap_or(other),
    };
    let key = if let Some(letter) = name.strip_prefix("Key").filter(|letter| letter.len() == 1) {
        Key::Character(letter.to_ascii_lowercase())
    } else if let Some(number) = name
        .strip_prefix("Numpad")
        .filter(|number| number.len() == 1 && number.as_bytes()[0].is_ascii_digit())
    {
        Key::Character(number.into())
    } else {
        logical
            .parse()
            .unwrap_or(Key::Named(NamedKey::Unidentified))
    };
    let modifier = match logical {
        "Control" => Modifiers::CONTROL,
        "Shift" => Modifiers::SHIFT,
        "Alt" => Modifiers::ALT,
        "Meta" => Modifiers::META,
        _ => Modifiers::empty(),
    };
    KeyboardEvent {
        state,
        key,
        code,
        location: if !modifier.is_empty() && name.ends_with("Left") {
            Location::Left
        } else if !modifier.is_empty() && name.ends_with("Right") {
            Location::Right
        } else if name.starts_with("Numpad") {
            Location::Numpad
        } else {
            Location::Standard
        },
        modifiers: if state.is_down() {
            modifier
        } else {
            Modifiers::empty()
        },
        repeat: false,
        is_composing: false,
    }
}

struct Runner {
    runtime: StoryRuntime,
    presentation: presentation::Presentation,
    audio: audio::AudioClock,
    saves: tempfile::TempDir,
    wake: WakeSignal,
    host: Rc<DeterministicEnvironment>,
    window: window::VirtualWindow,
    steps: u64,
    logical_ns: u64,
    started: Instant,
    actions: ActionTrace,
    pending_input: VecDeque<PhysicalInput>,
    pending_target: Option<String>,
    settling_waits: Option<Vec<aside_vm::executor::WaitId>>,
    waiting_activities: Vec<aside_vm::executor::WaitId>,
    progress_at: Option<Instant>,
}

struct DriveProgress {
    steps: u64,
    logical_ns: u64,
    accepted_frames: u64,
    interaction: Option<&'static str>,
    interaction_generation: Option<u64>,
    content_id: Option<String>,
    read_content: usize,
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
        let mut window = window::VirtualWindow::new();
        window.publish(&mut driver);
        let storage = SaveService::new(
            Arc::new(DirectorySaves::new(saves.path(), 256 << 20)),
            QueueLimits {
                max_requests: 16,
                max_bytes: 256 << 20,
            },
            wake.clone(),
        );
        let mut runtime = StoryRuntime::new(driver)
            .with_saves(RuntimeSaves::new(storage, 256 << 20))
            .with_media_factory(media);
        runtime.set_frame_capture_enabled(true);
        runtime.set_window_control_enabled(true);
        let presentation = presentation::Presentation::new(runtime.driver(), wake.clone())
            .map_err(|error| anyhow::anyhow!(error))?;
        Ok(Self {
            runtime,
            presentation,
            audio,
            saves,
            wake,
            host,
            window,
            steps: 0,
            logical_ns: 0,
            started: Instant::now(),
            actions: ActionTrace::default(),
            pending_input: VecDeque::new(),
            pending_target: None,
            settling_waits: None,
            waiting_activities: Vec::new(),
            progress_at: None,
        })
    }

    fn failure(&self, outcome: RunOutcome, message: impl Into<String>) -> DriveFailure {
        (outcome, diagnostic(&self.runtime, message.into()))
    }

    fn reset_presentation(&mut self) -> Result<(), DriveFailure> {
        self.presentation
            .reset(self.runtime.driver())
            .map_err(|error| self.failure(RunOutcome::ResourceFailure, error))?;
        self.pending_input.clear();
        self.pending_target = None;
        self.settling_waits = None;
        self.waiting_activities.clear();
        Ok(())
    }

    /// Rebuild the saved accepted input boundary without executing its successor.
    fn restored_decision(&mut self) -> Result<Option<Decision>, DriveFailure> {
        loop {
            self.steps += 1;
            let driver = self.runtime.driver_mut().map_err(|error| {
                (
                    RunOutcome::RuntimeFailure,
                    RunDiagnostic {
                        message: error.to_string(),
                        stack: Vec::new(),
                    },
                )
            })?;
            self.window.publish(driver);
            self.presentation.set_viewport(self.window.size());
            let ready = driver
                .prepare_restored(SliceBudget::default())
                .map_err(|error| {
                    (
                        RunOutcome::RuntimeFailure,
                        RunDiagnostic {
                            message: error.to_string(),
                            stack: Vec::new(),
                        },
                    )
                })?;
            if !ready {
                if driver.ui().assets().pending_io() && !self.wake.acknowledge() {
                    std::thread::park();
                }
                continue;
            }
            let stamp = driver.projection_stamp();
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
            if !presented {
                if driver.ui().assets().pending_io() {
                    if !self.wake.acknowledge() {
                        std::thread::park();
                    }
                    continue;
                }
                let ready = driver
                    .prepare_restored(SliceBudget::default())
                    .map_err(|error| {
                        (
                            RunOutcome::RuntimeFailure,
                            RunDiagnostic {
                                message: error.to_string(),
                                stack: Vec::new(),
                            },
                        )
                    })?;
                if !ready || driver.projection_stamp() != stamp {
                    continue;
                }
                return Err(self.failure(
                    RunOutcome::CheckpointUnavailable,
                    "restored input presentation remains unavailable after projection preparation",
                ));
            }
            if driver.narrative_input_ready()
                && let Some(InteractionView::Choice(choice)) =
                    driver.ui().session().view().interaction
            {
                return Ok(Some(Decision {
                    kind: ActionKind::Choice,
                    input_state: None,
                    option_keys: choice
                        .options()
                        .iter()
                        .map(|option| option.key().to_owned())
                        .collect(),
                    actions: choice
                        .options()
                        .iter()
                        .map(|option| {
                            DecisionAction::Choice(InputAction::Select(option.id().clone()))
                        })
                        .collect(),
                }));
            }
            let targets = driver.input_targets(&self.presentation.bridge, self.presentation.root);
            return Self::control_decision(driver, targets);
        }
    }

    fn control_decision(
        driver: &AsideSessionUi,
        targets: Vec<reel_player::session::InputTarget>,
    ) -> Result<Option<Decision>, DriveFailure> {
        if targets.is_empty() {
            return Ok(None);
        }
        let mut decision = Decision {
            kind: ActionKind::Controls,
            input_state: Some(control_input_state(&targets)),
            option_keys: targets.iter().map(|target| target.key.clone()).collect(),
            actions: targets
                .into_iter()
                .map(|target| DecisionAction::Control {
                    position: target.position,
                    text: target.accepts_text.then_some("Player"),
                    submit: target.submits_text,
                    binding: target
                        .binding
                        .map(|binding| (binding, target.binding_value)),
                })
                .collect(),
        };
        let waits = driver.adapter().finite_activity_waits().map_err(|error| {
            (
                RunOutcome::RuntimeFailure,
                RunDiagnostic {
                    message: error.to_string(),
                    stack: Vec::new(),
                },
            )
        })?;
        if !waits.is_empty() {
            decision.option_keys = std::iter::once(WAIT_FINITE.into())
                .chain(decision.option_keys.iter().cloned())
                .collect();
            decision
                .actions
                .insert(0, DecisionAction::WaitForActivities(waits));
        }
        Ok(Some(decision))
    }

    /// Run ordinary VM slices at the current virtual instant. Advance time only
    /// when the scheduler reaches a timed wait, and park only for real asset IO.
    fn drive(&mut self) -> Result<Option<Decision>, DriveFailure> {
        self.drive_inner(None)
    }

    fn drive_observed(
        &mut self,
        mut observer: impl FnMut(DriveProgress) -> Result<(), DriveFailure>,
    ) -> Result<Option<Decision>, DriveFailure> {
        self.drive_inner(Some(&mut observer))
    }

    fn drive_inner(
        &mut self,
        mut observer: Option<&mut dyn FnMut(DriveProgress) -> Result<(), DriveFailure>>,
    ) -> Result<Option<Decision>, DriveFailure> {
        loop {
            if let Some(observer) = observer.as_mut()
                && self
                    .progress_at
                    .is_none_or(|at| at.elapsed() >= Duration::from_secs(1))
            {
                self.progress_at = Some(Instant::now());
                let session = self.runtime.driver().ui().session();
                let (interaction, interaction_generation, content_id) =
                    match session.view().interaction.as_ref() {
                        Some(InteractionView::Dialogue(dialogue)) => (
                            Some("dialogue"),
                            Some(dialogue.id().generation()),
                            dialogue
                                .lines()
                                .last()
                                .map(|line| line.content_id().to_owned()),
                        ),
                        Some(InteractionView::Choice(choice)) => {
                            (Some("choice"), Some(choice.id().generation()), None)
                        }
                        None => (None, None, None),
                    };
                observer(DriveProgress {
                    steps: self.steps,
                    logical_ns: self.logical_ns,
                    accepted_frames: self.presentation.frames,
                    interaction,
                    interaction_generation,
                    content_id,
                    read_content: session.read_progress().len(),
                })?;
            }
            if !self.runtime.capturing() {
                let driver = self.runtime.driver_mut().map_err(|error| {
                    (
                        RunOutcome::RuntimeFailure,
                        RunDiagnostic {
                            message: error.to_string(),
                            stack: Vec::new(),
                        },
                    )
                })?;
                self.window.publish(driver);
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
            let vm = self.runtime.driver().adapter().vm();
            self.waiting_activities
                .retain(|&wait| vm.wait_is_pending(wait));
            let waiting_for_activities = !self.waiting_activities.is_empty();
            if !self.runtime.capturing() {
                let driver = self.runtime.driver_mut().map_err(|error| {
                    (
                        RunOutcome::RuntimeFailure,
                        RunDiagnostic {
                            message: error.to_string(),
                            stack: Vec::new(),
                        },
                    )
                })?;
                let capture = self.presentation.drive_capture(driver).map_err(|error| {
                    (
                        RunOutcome::ResourceFailure,
                        RunDiagnostic {
                            message: error,
                            stack: Vec::new(),
                        },
                    )
                })?;
                if capture.changed {
                    self.runtime.wake_input();
                }
                if capture.busy {
                    self.presentation
                        .wait_capture()
                        .map_err(|error| self.failure(RunOutcome::ResourceFailure, error))?;
                    continue;
                }
            }
            let mut submitted_input = false;
            if !self.runtime.capturing() {
                let idle = !self.runtime.needs_tick();
                let checkpoint_ready = self.runtime.checkpoint_ready()
                    && !self.runtime.driver().adapter().has_capture_requests();
                let driver = self.runtime.driver_mut().map_err(|error| {
                    (
                        RunOutcome::RuntimeFailure,
                        RunDiagnostic {
                            message: error.to_string(),
                            stack: Vec::new(),
                        },
                    )
                })?;
                self.presentation.set_viewport(self.window.size());
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
                    if self
                        .window
                        .drive(
                            driver,
                            &mut self.presentation.bridge,
                            self.presentation.root,
                        )
                        .map_err(|error| {
                            (
                                RunOutcome::RuntimeFailure,
                                RunDiagnostic {
                                    message: error,
                                    stack: Vec::new(),
                                },
                            )
                        })?
                    {
                        self.runtime.wake_input();
                        continue;
                    }
                    if driver.state() == StoryState::Finished && idle {
                        return Ok(None);
                    }
                    if let Some(mut input) = self.pending_input.pop_front() {
                        if matches!(input, PhysicalInput::Text(_))
                            && self.pending_target.as_ref().is_some_and(|key| {
                                !driver
                                    .input_targets(
                                        &self.presentation.bridge,
                                        self.presentation.root,
                                    )
                                    .iter()
                                    .any(|target| &target.key == key)
                            })
                        {
                            self.pending_input.clear();
                            self.runtime.wake_input();
                            continue;
                        }
                        if let PhysicalInput::Pointer(event) = &mut input
                            && let Some(key) = &self.pending_target
                        {
                            use reel_ui::{core::PointerEvent, dpi::PhysicalPosition};
                            let target = driver
                                .input_targets(&self.presentation.bridge, self.presentation.root)
                                .into_iter()
                                .find(|target| &target.key == key);
                            if let Some(target) = target {
                                let position =
                                    PhysicalPosition::new(target.position[0], target.position[1]);
                                match event {
                                    PointerEvent::Move(update) => {
                                        update.current.position = position
                                    }
                                    PointerEvent::Down(button) | PointerEvent::Up(button) => {
                                        button.state.position = position
                                    }
                                    _ => {}
                                }
                            } else {
                                let pointer = match event {
                                    PointerEvent::Move(update) => update.pointer,
                                    PointerEvent::Down(button) | PointerEvent::Up(button) => {
                                        button.pointer
                                    }
                                    _ => unreachable!("queued click event"),
                                };
                                *event = PointerEvent::Cancel(pointer);
                                self.pending_input.clear();
                            }
                        }
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
                        driver
                            .publish_pointer_position(
                                &self.presentation.bridge,
                                self.presentation.root,
                            )
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
                        && !waiting_for_activities
                        && !driver.has_runnable_tasks()
                        && !driver.activities_pending()
                    {
                        if driver.narrative_input_ready() {
                            match driver.ui().session().view().interaction {
                                Some(InteractionView::Choice(choice)) => {
                                    let decision = Decision {
                                        kind: ActionKind::Choice,
                                        input_state: None,
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
                                    };
                                    if self.runtime.checkpoint_ready()
                                        && !self.runtime.driver().adapter().has_capture_requests()
                                    {
                                        return Ok(Some(decision));
                                    }
                                    continue;
                                }
                                Some(InteractionView::Dialogue(dialogue))
                                    if (dialogue.reveal_skippable()
                                        && dialogue.lines().iter().any(|line| {
                                            !line.playback_complete() || !line.advance_requested()
                                        }))
                                        || (dialogue.awaiting_advance()
                                            && dialogue
                                                .lines()
                                                .iter()
                                                .all(|line| line.playback_complete())) =>
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
                                        .is_some_and(|outcome| {
                                            outcome != reel_engine::InputOutcome::Pending
                                        });
                                }
                                _ => {}
                            }
                        }
                        if !submitted_input
                            && (!driver.narrative_input_ready()
                                || driver.ui().session().view().interaction.is_none()
                                || matches!(driver.ui().session().view().interaction,
                                    Some(InteractionView::Dialogue(dialogue))
                                        if !dialogue.awaiting_advance() && dialogue.lines().iter().all(|line|
                                            line.playback_complete() && (!dialogue.reveal_skippable() || line.advance_requested()))))
                        {
                            let targets = driver
                                .input_targets(&self.presentation.bridge, self.presentation.root);
                            if let Some(decision) = Self::control_decision(driver, targets)? {
                                let repeated = self.actions.last().is_some_and(|action| {
                                    action.kind == ActionKind::Controls
                                        && action.option_keys == decision.option_keys
                                        && action.option_keys[action.selected] != WAIT_FINITE
                                });
                                let settling =
                                    if repeated {
                                        if self.settling_waits.is_none() {
                                            self.settling_waits = Some(
                                                driver.adapter().finite_activity_waits().map_err(
                                                    |error| {
                                                        (
                                                            RunOutcome::RuntimeFailure,
                                                            RunDiagnostic {
                                                                message: error.to_string(),
                                                                stack: Vec::new(),
                                                            },
                                                        )
                                                    },
                                                )?,
                                            );
                                        }
                                        self.settling_waits.as_ref().unwrap().iter().any(|&wait| {
                                            driver.adapter().vm().wait_is_pending(wait)
                                        })
                                    } else {
                                        false
                                    };
                                if !settling {
                                    if self.runtime.checkpoint_ready()
                                        && !self.runtime.driver().adapter().has_capture_requests()
                                    {
                                        return Ok(Some(decision));
                                    }
                                    continue;
                                }
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
        self.settling_waits = None;
        self.pending_target = None;
        self.waiting_activities.clear();
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
            DecisionAction::WaitForActivities(waits) => {
                self.waiting_activities.clone_from(waits);
            }
            DecisionAction::Control {
                position: [x, y],
                text,
                submit,
                binding,
            } => {
                self.pending_target = Some(decision.option_keys[selected].clone());
                use reel_ui::{core::*, dpi::PhysicalPosition};
                if let Some((binding, value)) = binding
                    && !matches!(binding.control, reel_ui::input::Control::Pointer(_))
                {
                    use reel_ui::input::{Control, InputDeviceId, InputEvent};
                    let device = if matches!(binding.control, Control::Key(_)) {
                        InputDeviceId::Keyboard
                    } else {
                        InputDeviceId::Gamepad(1)
                    };
                    for value in [*value, 0.] {
                        let event = if let Control::Key(code) = binding.control {
                            TextEvent::Keyboard(keyboard_event(
                                code,
                                if value == 0. {
                                    keyboard::KeyState::Up
                                } else {
                                    keyboard::KeyState::Down
                                },
                            ))
                        } else {
                            TextEvent::Input(InputEvent::Value {
                                device,
                                control: binding.control,
                                value,
                            })
                        };
                        self.pending_input.push_back(PhysicalInput::Text(event));
                    }
                } else {
                    let button = binding
                        .and_then(|(binding, _)| {
                            if let reel_ui::input::Control::Pointer(button) = binding.control {
                                Some(button)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(PointerButton::Primary);
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
                            button: Some(button),
                        })),
                        PhysicalInput::Pointer(PointerEvent::Up(PointerButtonEvent {
                            pointer,
                            state,
                            button: Some(button),
                        })),
                    ]);
                }
                if let Some(text) = text {
                    self.pending_input
                        .push_back(PhysicalInput::Text(TextEvent::ClipboardPaste(
                            (*text).into(),
                        )));
                }
                if *submit {
                    for state in [keyboard::KeyState::Down, keyboard::KeyState::Up] {
                        self.pending_input
                            .push_back(PhysicalInput::Text(TextEvent::Keyboard(
                                keyboard::KeyboardEvent {
                                    state,
                                    key: keyboard::Key::Named(keyboard::NamedKey::Enter),
                                    code: keyboard::Code::Enter,
                                    location: keyboard::Location::Standard,
                                    modifiers: keyboard::Modifiers::empty(),
                                    repeat: false,
                                    is_composing: false,
                                },
                            )));
                    }
                }
            }
        }
        self.actions.push(ActionSelection {
            kind: decision.kind,
            option_keys: decision.option_keys.clone(),
            input_state: decision.input_state.clone(),
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
                || expected.input_state != decision.input_state
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
