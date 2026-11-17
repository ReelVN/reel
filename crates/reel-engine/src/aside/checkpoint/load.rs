//! Bounded loading into a fresh session using the current initializer and Root.
use super::{
    CheckpointDataRestore, CheckpointError, CheckpointRecord, CheckpointResumeCursor,
    RestoredCheckpoint, flow::FlowPreparation, interaction::is_reading_wait,
};
use crate::{
    Session,
    aside::{
        NativeBudget, NativeDispatch, RunOutcome, SessionBudget, SessionStep, SessionVm,
        SliceBudget,
    },
    reading::checkpoint::SavedReading,
};
use aside_vm::{GcBudget, GcPhase, HeapReservation, executor::NativeInput, resume::ResumeProgress};
use std::sync::Arc;
#[cfg(test)]
mod tests;

enum Phase {
    Initialize,
    Registry,
    Data,
    Resources,
    Bootstrap,
    Frames,
    Reading,
    Operation,
    Adopt,
    Complete,
}

/// A movable cursor for one isolated candidate. The host retains the candidate
/// session and adapter until loading finishes or the candidate is discarded.
pub struct CheckpointLoad {
    phase: Phase,
    record: Arc<CheckpointRecord>,
    reading: Option<SavedReading>,
    data: Option<CheckpointDataRestore>,
    restored: Option<RestoredCheckpoint>,
    frames: Option<CheckpointResumeCursor>,
    charge: HeapReservation,
    proof: Option<(crate::aside::HostTicket, bool)>,
    required_work: u32,
    retried_collection: bool,
}

impl CheckpointLoad {
    /// Begin after loading and starting the current program's initializer.
    pub fn new(
        mut record: CheckpointRecord,
        candidate: &SessionVm,
    ) -> Result<Self, CheckpointError> {
        let charge = candidate
            .vm
            .heap()
            .budget_usage()
            .reserve(record.retained_bytes() + size_of::<Self>() as u64)?;
        let reading = record.take_reading();
        Ok(Self {
            phase: Phase::Initialize,
            record: Arc::new(record),
            reading,
            data: None,
            restored: None,
            frames: None,
            charge,
            proof: None,
            required_work: 1,
            retried_collection: false,
        })
    }

    /// Minimum native work required by the next current-code stack proof.
    pub fn required_work(&self) -> u32 {
        self.required_work
    }

    /// Whether another step can run without an external wake.
    pub fn ready(&self, candidate: &SessionVm) -> bool {
        if candidate.vm.heap().phase() != GcPhase::Idle {
            return true;
        }
        match self.phase {
            Phase::Initialize | Phase::Bootstrap | Phase::Operation => {
                candidate.has_runnable_tasks()
            }
            Phase::Complete => false,
            _ => true,
        }
    }

    fn step_vm(
        &mut self,
        candidate: &mut SessionVm,
        session: &mut Session,
        budget: SliceBudget,
        now: impl FnMut() -> u64,
    ) -> Result<SessionStep, CheckpointError> {
        let step = candidate.step(session, SessionBudget::RetryMinimum(budget), now)?;
        match step {
            SessionStep::Vm(RunOutcome::ResourceBlocked(blocked)) => {
                if self.retried_collection {
                    return Err(CheckpointError::ResourceBlocked(blocked));
                }
                candidate.begin_collection()?;
                self.retried_collection = true;
                Ok(SessionStep::Idle)
            }
            SessionStep::Vm(RunOutcome::BudgetYield(_)) | SessionStep::Idle => Ok(step),
            _ => {
                self.retried_collection = false;
                Ok(step)
            }
        }
    }

    /// Advance one bounded host turn. The host services candidate asset,
    /// localization, media and persistent-data requests between these turns.
    pub fn step(
        &mut self,
        candidate: &mut SessionVm,
        session: &mut Session,
        locale: &str,
        native_budget: NativeBudget,
        chunk: usize,
        budget: SliceBudget,
        now: impl FnMut() -> u64,
    ) -> Result<bool, CheckpointError> {
        if candidate.vm.heap().phase() != GcPhase::Idle {
            candidate.gc_step(GcBudget::default())?;
            return Ok(false);
        }
        let work = native_budget.with_minimum(1);
        match self.phase {
            Phase::Initialize => match self.step_vm(candidate, session, budget, now)? {
                SessionStep::Vm(RunOutcome::Returned(_)) => {
                    candidate.discard_delivery(session)?;
                    self.phase = Phase::Registry;
                }
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    candidate.dispatch(session, ticket, locale, native_budget)?;
                }
                SessionStep::Vm(RunOutcome::Fault(_)) => {
                    return Err(CheckpointError::Contract(
                        "current module initializer failed while loading",
                    ));
                }
                _ => {}
            },
            Phase::Registry => {
                if matches!(
                    candidate.prepare_flow_registry(NativeBudget::Exact(work))?,
                    NativeDispatch::Ready { .. }
                ) {
                    // The data cursor accounts for the shared record until adoption.
                    self.charge.resize(
                        size_of::<Self>() as u64
                            + self
                                .reading
                                .as_ref()
                                .map_or(0, SavedReading::retained_bytes),
                    )?;
                    self.data = Some(candidate.restore_checkpoint_data_begin(self.record.clone())?);
                    self.phase = Phase::Data;
                }
            }
            Phase::Data => {
                if self.data.as_mut().unwrap().step(candidate, work, chunk)? {
                    self.restored = Some(self.data.take().unwrap().finish(candidate)?);
                    self.phase = Phase::Resources;
                }
            }
            Phase::Resources => {
                if candidate.prepare_checkpoint_resources(work)? {
                    if let Some(reading) = &self.reading {
                        session.reserve_interaction_generations(reading)?;
                    }
                    let state = self.restored.as_ref().unwrap().root_state(&candidate.vm)?;
                    candidate.start_root(session, Some(state))?;
                    self.phase = Phase::Bootstrap;
                }
            }
            Phase::Bootstrap => match self.step_vm(candidate, session, budget, now)? {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    let p = candidate.vm.program().package();
                    let symbol = &p.symbols[p.natives[ticket.native.index()].symbol.index()];
                    if symbol == "reel::run_flow" && ticket.input == NativeInput::Entry {
                        if let FlowPreparation::Ready(_) =
                            candidate.prepare_flow_entry(session, ticket, native_budget)?
                        {
                            self.frames = Some(candidate.resume_checkpoint_begin(
                                ticket,
                                self.restored.as_ref().unwrap(),
                            )?);
                            self.phase = Phase::Frames;
                        }
                    } else {
                        candidate.dispatch(session, ticket, locale, native_budget)?;
                    }
                }
                SessionStep::Vm(RunOutcome::Returned(_) | RunOutcome::Fault(_)) => {
                    return Err(CheckpointError::Contract(
                        "current Root ended before entering the saved business flow",
                    ));
                }
                _ => {}
            },
            Phase::Frames => {
                if matches!(
                    self.frames.as_mut().unwrap().step(
                        &mut candidate.vm,
                        self.restored.as_ref().unwrap(),
                        work,
                        budget,
                        now
                    )?,
                    ResumeProgress::Complete(_)
                ) {
                    self.frames.take().unwrap().finish(&mut candidate.vm)?;
                    candidate.issued = None;
                    if let Some(reading) = self.reading.take() {
                        candidate
                            .prepare_reading_resume(reading, self.record.continuation().clone())?;
                    }
                    self.phase = Phase::Reading;
                }
            }
            Phase::Reading => {
                if candidate.step_prepared_reading(session, work, chunk)? {
                    self.phase = Phase::Operation;
                }
            }
            Phase::Operation => {
                match self.step_vm(candidate, session, budget, now)? {
                    SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                        let reading = candidate.reading_resume_pending();
                        let p = candidate.vm.program().package();
                        let symbol = &p.symbols[p.natives[ticket.native.index()].symbol.index()];
                        // The Reading adapter proves its own ticket before reconnecting a wait.
                        let binds_reading = reading && is_reading_wait(symbol);
                        let boundary = candidate.flow_boundary().filter(|&(task, _)| {
                            !binds_reading
                                && ticket.task == task
                                && ticket.input == NativeInput::Entry
                        });
                        let at_point = match (boundary, self.proof) {
                            (None, _) => false,
                            (Some(_), Some((proved, matches))) if proved == ticket => matches,
                            (Some((_, boundary)), _) => {
                                let bound = candidate
                                    .vm
                                    .resume_descriptor_bound(&candidate.resume, ticket)?;
                                self.required_work = u32::try_from(
                                    bound.work.saturating_add(bound.key_bytes),
                                )
                                .map_err(|_| {
                                    CheckpointError::Contract(
                                        "saved operation proof exceeds native work capacity",
                                    )
                                })?;
                                if native_budget.with_minimum(self.required_work)
                                    < self.required_work
                                {
                                    return Ok(false);
                                }
                                let matches = candidate
                                    .vm
                                    .resume_descriptor(
                                        &candidate.resume,
                                        ticket,
                                        boundary,
                                        aside_vm::resume::ResumeLimits {
                                            work: bound.work,
                                            ..Default::default()
                                        },
                                    )
                                    .is_ok_and(|descriptor| {
                                        descriptor.matches(self.record.continuation())
                                    });
                                self.proof = Some((ticket, matches));
                                self.required_work = 1;
                                return Ok(false);
                            }
                        };
                        if reading && at_point {
                            return Err(CheckpointError::Contract(
                                "current point operation differs from the saved reading interaction",
                            ));
                        }
                        let dispatched =
                            candidate.dispatch(session, ticket, locale, native_budget)?;
                        if (at_point || binds_reading)
                            && matches!(
                                dispatched,
                                NativeDispatch::Ready { .. } | NativeDispatch::Published { .. }
                            )
                            && !candidate.reading_resume_pending()
                        {
                            // The restored point is a presentation boundary.
                            // Implicit native waits have no await activation,
                            // and a ready result must leave its successor paused.
                            let wait = ticket.wait_id();
                            candidate.main_wait =
                                candidate.vm.wait_is_pending(wait).then_some(wait);
                            if !candidate.reactive_ready {
                                candidate.finish_script_batch(session, ticket.task, true)?;
                            }
                            self.phase = Phase::Adopt;
                        }
                    }
                    SessionStep::Vm(RunOutcome::Fault(_) | RunOutcome::Returned(_)) => {
                        return Err(CheckpointError::Contract(
                            "saved business point failed before reconnecting its operation",
                        ));
                    }
                    _ => {}
                }
            }
            Phase::Adopt => {
                if candidate.adopt_checkpoint_resources(work)? {
                    self.restored.take().unwrap().release(&mut candidate.vm)?;
                    self.phase = Phase::Complete;
                }
            }
            Phase::Complete => return Ok(true),
        }
        Ok(matches!(self.phase, Phase::Complete))
    }
}
