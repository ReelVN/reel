//! Compiler/VM-to-SessionUi assembly shared by library hosts and the native window.
use crate::session::{ProjectionStamp, SessionUi, SessionWidget};
use reel_engine::aside::{
    DeliveryId, FaultInfo, GcBudget, GcProgress, NativeDispatch, RunOutcome, SessionStep,
    SessionVm, SliceBudget, VmLimits, source,
};
use reel_engine::{InteractionId, InteractionView, Session};
use reel_ui::{
    bridge::PresentationBridge,
    core::{Handled, PointerEvent, WidgetId},
};

type Error = Box<dyn std::error::Error>;

#[derive(Debug)]
pub enum StoryProgress {
    /// No VM task is ready; mounted interactive nodes remain usable.
    Idle,
    /// A node callback completed or its cancellation made bounded progress.
    CallbackProgress,
    /// Initializer result was discarded before starting the zero-argument entry.
    Initialized,
    Interaction(InteractionId),
    Decoding {
        work: u32,
    },
    /// A synchronous native operation updated the author stage.
    Updated {
        work: u32,
    },
    /// A host admission/argument/effect error was delivered at the native call.
    NativeFault(String),
    /// Nonterminal Waiting, BudgetYield and ResourceBlocked preserve VM IDs.
    Vm(RunOutcome),
    HostFailed(HostFailureStage),
    Faulted(DeliveryId),
    Finished,
    Cancelled,
}

/// Fault delivery remains rooted until explicit cancellation and cleanup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoryState {
    Running,
    StartingEntry,
    Finishing,
    HostFailed(HostFailureStage),
    Faulted(DeliveryId),
    Finished,
    Cancelled,
}

/// Host failures never create a language Fault delivery. Fixed VM limits cannot
/// be enlarged through this driver; inspect the error, then cancel and clean up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostFailureStage {
    Drive,
    StartEntry,
    Finish,
    Cancel,
}

#[derive(Debug)]
pub struct HostFailure {
    pub stage: HostFailureStage,
    pub error: Error,
}

/// Owns both sides so UI cancel cannot invoke the pure Rust zero-obligation
/// shortcut. All rendering/presented-input behavior remains in SessionUi.
pub struct AsideSessionUi {
    ui: SessionUi,
    vm: SessionVm,
    entry: Option<source::StoryFunction>,
    locale: String,
    state: StoryState,
    host_failure: Option<HostFailure>,
}

impl AsideSessionUi {
    pub fn from_source(
        session: Session,
        text: &str,
        module: &str,
        version: u64,
        entry_name: &str,
        locale: &str,
        limits: VmLimits,
    ) -> Result<Self, Error> {
        let story = source::compile_story(text, module, version, entry_name)?;
        Self::from_compiled(session, story, locale, limits)
    }

    /// Assemble a verified bytecode asset with its host-selected entry metadata.
    /// B01 decoding happens upstream; trusted native admission happens here.
    /// Construction errors return no driver; the owned VM and session are dropped.
    pub fn from_compiled(
        session: Session,
        story: source::CompiledStory,
        locale: &str,
        limits: VmLimits,
    ) -> Result<Self, Error> {
        if locale.is_empty() {
            return Err("empty page locale".into());
        }
        story.validate()?;
        let mut ui = SessionUi::new(session)?;
        let mut vm = SessionVm::load(ui.session(), story.program, limits)?;
        vm.start(ui.session(), story.initializer)?;
        ui.set_stage(vm.stage());
        Ok(Self {
            ui,
            vm,
            entry: Some(story.entry),
            locale: locale.into(),
            state: StoryState::Running,
            host_failure: None,
        })
    }
    pub fn state(&self) -> StoryState {
        self.state
    }
    /// Select asset resolution and memory policy before driving the story.
    pub fn with_asset_options(mut self, options: crate::assets::AssetOptions) -> Self {
        self.ui = self.ui.with_asset_options(options);
        self
    }
    pub fn with_assets(mut self, assets: crate::assets::Assets) -> Self {
        self.ui = self.ui.with_assets(assets);
        self
    }
    /// Install the host's completion wake-up before requesting resources.
    /// Adapters may call it from a worker; synchronize on the host thread.
    pub fn set_resource_waker(&mut self, wake: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.ui.set_resource_waker(wake);
    }
    pub fn host_failure(&self) -> Option<&HostFailure> {
        self.host_failure.as_ref()
    }
    fn fail_host(&mut self, stage: HostFailureStage, error: Error) -> StoryProgress {
        self.host_failure = Some(HostFailure { stage, error });
        self.state = StoryState::HostFailed(stage);
        self.ui.clear_presented();
        StoryProgress::HostFailed(stage)
    }
    /// Ephemeral inspection: valid while Faulted, including across GC ticks.
    /// Cancellation invalidates the delivery; this does not create a host root.
    pub fn fault(&self) -> Result<Option<FaultInfo>, Error> {
        match self.state {
            StoryState::Faulted(id) => Ok(self.vm.vm().delivery_fault(id)?),
            _ => Ok(None),
        }
    }
    pub fn ui(&self) -> &SessionUi {
        &self.ui
    }
    pub(crate) fn ui_mut(&mut self) -> &mut SessionUi {
        &mut self.ui
    }
    pub fn adapter(&self) -> &SessionVm {
        &self.vm
    }
    pub fn cleanup_pending(&self) -> bool {
        self.vm.cleanup_pending()
    }
    pub fn activities_need_time(&self) -> bool {
        self.state == StoryState::Running && self.vm.activities_need_time()
    }
    pub fn activities_pending(&self) -> bool {
        self.vm.activities_pending()
    }
    /// Logical seconds supplied by the host, with an explicit activity work quota.
    pub fn advance_time(
        &mut self,
        seconds: f64,
        work: u32,
    ) -> Result<reel_engine::aside::ActivityProgress, Error> {
        if self.state != StoryState::Running {
            return Ok(Default::default());
        }
        Ok(self.vm.advance_time(self.ui.session(), seconds, work)?)
    }
    pub fn begin_collection(&mut self) -> Result<(), Error> {
        Ok(self.vm.begin_collection()?)
    }
    pub fn gc_step(&mut self, budget: GcBudget) -> Result<GcProgress, Error> {
        Ok(self.vm.gc_step(budget)?)
    }
    pub fn widget(&self) -> SessionWidget {
        self.ui.widget()
    }
    pub fn projection_stamp(&self) -> ProjectionStamp {
        self.ui.projection_stamp()
    }
    pub fn presented(&mut self, stamp: ProjectionStamp) -> Result<(), Error> {
        self.ui.presented(stamp);
        let actions = self.ui.take_actions();
        self.accept_actions(actions)
    }
    pub fn clear_presented(&mut self) -> Result<(), Error> {
        self.ui.clear_presented();
        let actions = self.ui.take_actions();
        self.accept_actions(actions)
    }

    pub fn sync(&mut self, bridge: &mut PresentationBridge, root: WidgetId) -> Result<(), Error> {
        if self.vm.activities_pending() {
            return Ok(());
        }
        if !matches!(self.state, StoryState::Finished | StoryState::Cancelled) {
            self.vm.sync_stage(self.ui.session())?;
            self.ui.set_stage(self.vm.stage());
        }
        self.sync_input(bridge, root)
    }
    /// Physical input updates session/focus state and schedules callbacks. No
    /// script has executed yet, so do not scan or reproject the script stage.
    pub(crate) fn sync_input(
        &mut self,
        bridge: &mut PresentationBridge,
        root: WidgetId,
    ) -> Result<(), Error> {
        self.ui.sync(bridge, root)?;
        let actions = self.ui.take_actions();
        self.accept_actions(actions)
    }
    pub fn handle_pointer(
        &mut self,
        bridge: &mut PresentationBridge,
        event: PointerEvent,
    ) -> Result<Handled, Error> {
        let result = self.ui.handle_pointer(bridge, event)?;
        self.accept_ui_input(result)
    }
    pub fn handle_text(
        &mut self,
        bridge: &mut PresentationBridge,
        event: reel_ui::core::TextEvent,
    ) -> Result<Handled, Error> {
        let result = self.ui.handle_text(bridge, event)?;
        self.accept_ui_input(result)
    }
    fn accept_ui_input(&mut self, result: crate::session::UiDispatch) -> Result<Handled, Error> {
        if self.state == StoryState::Running
            && let Some(click) = result.click
        {
            self.vm.click(self.ui.session_mut(), click.node)?;
        }
        self.accept_actions(result.actions)?;
        Ok(result.handled)
    }
    fn accept_actions(&mut self, actions: Vec<reel_ui::stage::NodeAction>) -> Result<(), Error> {
        if self.state == StoryState::Running {
            for action in &actions {
                self.vm.input_action(self.ui.session_mut(), action)?;
            }
        }
        self.ui.recycle_actions(actions);
        Ok(())
    }
    pub fn has_runnable_tasks(&self) -> bool {
        self.state == StoryState::Running && self.vm.has_runnable_tasks()
    }
    pub fn reveal_more(&mut self, graphemes: usize) -> Result<(), Error> {
        if let Some(InteractionView::Dialogue(dialogue)) = self.ui.session().view().interaction {
            let id = dialogue.id();
            let owner = self.ui.session().root_owner();
            self.ui.session_mut().reveal_more(owner, id, graphemes)?;
        }
        Ok(())
    }

    /// At most one bounded VM slice per call. Host failures become inspectable
    /// HostFailed progress; terminal calls perform no VM work. A host call never recursively
    /// executes its continuation; the next tick resumes it. Caller syncs and
    /// presents the new projection before allowing replacement UI input.
    pub fn tick(
        &mut self,
        budget: SliceBudget,
        host_work: u32,
        now: impl FnMut() -> u64,
    ) -> Result<StoryProgress, Error> {
        match self.state {
            StoryState::HostFailed(stage) => return Ok(StoryProgress::HostFailed(stage)),
            StoryState::Faulted(id) => return Ok(StoryProgress::Faulted(id)),
            StoryState::Finished => return Ok(StoryProgress::Finished),
            StoryState::Cancelled => return Ok(StoryProgress::Cancelled),
            StoryState::Running | StoryState::StartingEntry | StoryState::Finishing => {}
        }
        match self.drive(budget, host_work, now) {
            Ok(progress) => Ok(progress),
            Err(error) => {
                let stage = match self.state {
                    StoryState::StartingEntry => HostFailureStage::StartEntry,
                    StoryState::Finishing => HostFailureStage::Finish,
                    _ => HostFailureStage::Drive,
                };
                Ok(self.fail_host(stage, error))
            }
        }
    }

    fn drive(
        &mut self,
        budget: SliceBudget,
        host_work: u32,
        now: impl FnMut() -> u64,
    ) -> Result<StoryProgress, Error> {
        let outcome = match self.vm.step(self.ui.session_mut(), budget, now)? {
            SessionStep::Vm(outcome) => outcome,
            SessionStep::Progress => return Ok(StoryProgress::CallbackProgress),
            SessionStep::Idle => {
                if self.entry.is_none() && self.vm.task().is_none() {
                    self.vm.sync_stage(self.ui.session())?;
                    if !self.vm.has_live_work() {
                        return self.finish_story();
                    }
                }
                return Ok(StoryProgress::Idle);
            }
        };
        match outcome {
            RunOutcome::Returned(_) => {
                self.vm.discard_delivery(self.ui.session_mut())?;
                if let Some(entry) = self.entry {
                    self.state = StoryState::StartingEntry;
                    self.vm.start(self.ui.session(), entry)?;
                    self.entry = None;
                    self.state = StoryState::Running;
                    Ok(StoryProgress::Initialized)
                } else {
                    self.vm.sync_stage(self.ui.session())?;
                    if self.vm.has_live_work() {
                        Ok(StoryProgress::CallbackProgress)
                    } else {
                        self.finish_story()
                    }
                }
            }
            RunOutcome::HostCall(ticket) => {
                match self
                    .vm
                    .dispatch(self.ui.session_mut(), ticket, &self.locale, host_work)
                {
                    Ok(NativeDispatch::Published { interaction, .. }) => {
                        Ok(StoryProgress::Interaction(interaction))
                    }
                    Ok(NativeDispatch::Decoding { work }) => Ok(StoryProgress::Decoding { work }),
                    Ok(NativeDispatch::Ready { work }) => Ok(StoryProgress::Updated { work }),
                    Err(error) => {
                        let Some(kind) = error.native_fault_kind() else {
                            return Err(error.into());
                        };
                        self.vm.reject_native(self.ui.session(), ticket, kind)?;
                        Ok(StoryProgress::NativeFault(error.to_string()))
                    }
                }
            }
            RunOutcome::Fault(id) => {
                self.state = StoryState::Faulted(id);
                self.ui.clear_presented();
                Ok(StoryProgress::Faulted(id))
            }
            other => Ok(StoryProgress::Vm(other)),
        }
    }

    fn finish_story(&mut self) -> Result<StoryProgress, Error> {
        self.state = StoryState::Finishing;
        self.vm.sync_stage(self.ui.session())?;
        self.ui.set_stage(self.vm.stage());
        self.vm.finish(self.ui.session_mut())?;
        self.state = StoryState::Finished;
        self.ui.clear_presented();
        Ok(StoryProgress::Finished)
    }

    pub fn cancel(&mut self) -> Result<(), Error> {
        self.ui.close_resources();
        if matches!(self.state, StoryState::Finished | StoryState::Cancelled) {
            return Ok(());
        }
        self.ui.clear_presented();
        if let Err(error) = self.vm.revoke(self.ui.session_mut()) {
            let message = error.to_string();
            self.fail_host(HostFailureStage::Cancel, error.into());
            return Err(message.into());
        }
        self.entry = None;
        self.state = StoryState::Cancelled;
        Ok(())
    }
    pub fn cleanup_step(
        &mut self,
        root_work: u32,
        budget: SliceBudget,
        now: impl FnMut() -> u64,
    ) -> Result<u32, Error> {
        Ok(self
            .vm
            .cleanup_step(self.ui.session_mut(), root_work, budget, now)?)
    }
}

#[cfg(test)]
mod tests;
