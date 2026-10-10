//! Actual compiler/VM-to-SessionUi assembly. Runtime acceptance is pending;
//! this source is not executed by the existing Rust diagnostic window.
use crate::session::{ProjectionStamp, SessionUi, SessionWidget};
use reel_engine::aside::{
    DeliveryId, FaultInfo, GcBudget, GcProgress, NativeDispatch, RunOutcome, SessionVm,
    SliceBudget, VmLimits, source,
};
use reel_engine::{InteractionId, InteractionView, Session};
use reel_ui::{
    bridge::PresentationBridge,
    core::{Handled, PointerEvent, WidgetId},
};

type Error = Box<dyn std::error::Error>;

#[derive(Debug)]
pub enum StoryProgress {
    /// Initializer result was discarded before starting the zero-argument entry.
    Initialized,
    Interaction(InteractionId),
    Decoding {
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
        let ui = SessionUi::new(session)?;
        let mut vm = SessionVm::load(ui.session(), story.program, limits)?;
        vm.start(ui.session(), story.initializer)?;
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
    pub fn adapter(&self) -> &SessionVm {
        &self.vm
    }
    pub fn cleanup_pending(&self) -> bool {
        self.vm.cleanup_pending()
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
    pub fn presented(&mut self, stamp: ProjectionStamp) {
        self.ui.presented(stamp);
    }
    pub fn clear_presented(&mut self) {
        self.ui.clear_presented();
    }

    pub fn sync(&mut self, bridge: &mut PresentationBridge, root: WidgetId) -> Result<(), Error> {
        self.ui.sync(bridge, root)
    }
    pub fn handle_pointer(
        &mut self,
        bridge: &mut PresentationBridge,
        event: PointerEvent,
    ) -> Result<Handled, Error> {
        self.ui.handle_pointer(bridge, event)
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
        let outcome = self.vm.step(self.ui.session_mut(), budget, now)?;
        match outcome {
            RunOutcome::Returned(_) => {
                self.vm.discard_delivery(self.ui.session())?;
                if let Some(entry) = self.entry {
                    self.state = StoryState::StartingEntry;
                    self.vm.start(self.ui.session(), entry)?;
                    self.entry = None;
                    self.state = StoryState::Running;
                    Ok(StoryProgress::Initialized)
                } else {
                    self.state = StoryState::Finishing;
                    self.vm.finish(self.ui.session_mut())?;
                    self.state = StoryState::Finished;
                    self.ui.clear_presented();
                    Ok(StoryProgress::Finished)
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

    pub fn cancel(&mut self) -> Result<(), Error> {
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
