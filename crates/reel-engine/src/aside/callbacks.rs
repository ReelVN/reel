//! Node-owned tasks share the VM and its bounded slices with the story task.
use super::*;
use aside_vm::ValueTag;
use reel_ui::stage::NodeId;

pub(super) struct Callback {
    pub task: TaskId,
    pub owner: OwnerHandle,
    pub node: NodeId,
    pub wait: Option<WaitId>,
    pub cancelling: bool,
}

impl SessionVm {
    /// Publish input feedback before dispatching callbacks. A batch uses one stage sync.
    pub fn interactions(
        &mut self,
        session: &Session,
        events: &[reel_ui::stage::NodeEvent],
    ) -> Result<bool, AdapterError> {
        self.live(session)?;
        let pending = self.vm.heap().derived_pending();
        let mut changed = false;
        for event in events {
            if let reel_ui::stage::NodeEvent::Interaction(event) = event {
                changed |= self.stage.interaction(self.vm.heap_mut(), event)?;
            }
        }
        if changed {
            self.feedback_only = self.reactive_ready && !pending && self.derived_task.is_none();
            self.stage.sync(self.vm.heap_mut(), self.read_limit)?;
        }
        Ok(changed)
    }

    /// Called after the host routes a control activation through accepted geometry.
    /// Current node liveness, ancestor flags and modal scope are checked before task creation.
    pub fn click(&mut self, session: &mut Session, node: NodeId) -> Result<bool, AdapterError> {
        self.live(session)?;
        let Some((target, closure)) = self.stage.click(self.vm.heap(), node)? else {
            return Ok(false);
        };
        let event = self.vm.heap_mut().alloc_table()?;
        let key = self.vm.heap_mut().alloc_string("target")?;
        self.vm.heap_mut().table_store(event, 0, key, target)?;
        self.start_callback(session, node, closure, event)
    }

    pub fn input_action(
        &mut self,
        session: &mut Session,
        action: &reel_ui::stage::NodeAction,
    ) -> Result<bool, AdapterError> {
        self.live(session)?;
        let Some((target, closure)) = self.stage.input_action(self.vm.heap(), action)? else {
            return Ok(false);
        };
        use reel_ui::input::ActionPhase;
        let phase = match action.phase {
            ActionPhase::Pressed => "pressed",
            ActionPhase::Changed => "changed",
            ActionPhase::Repeated => "repeated",
            ActionPhase::Released => "released",
            ActionPhase::Cancelled => "cancelled",
        };
        let heap = self.vm.heap_mut();
        let event = heap.alloc_table()?;
        let name = heap.alloc_string(&action.action)?;
        let phase = heap.alloc_string(phase)?;
        for (i, (name, value)) in [
            ("target", target),
            ("action", name),
            ("value", Value::float(action.value.into())),
            ("phase", phase),
        ]
        .into_iter()
        .enumerate()
        {
            let key = heap.alloc_string(name)?;
            heap.table_store(event, i as u32, key, value)?;
        }
        self.start_callback(session, action.node, closure, event)
    }

    /// Commit the user value before scheduling the node-owned callback.
    pub fn change(
        &mut self,
        session: &mut Session,
        change: &reel_ui::stage::NodeChange,
    ) -> Result<bool, AdapterError> {
        self.live(session)?;
        let Some((target, closure, value)) = self.stage.change(self.vm.heap_mut(), change)? else {
            return Ok(false);
        };
        self.stage.sync(self.vm.heap_mut(), self.read_limit)?;
        if closure.tag() != ValueTag::Null {
            let heap = self.vm.heap_mut();
            let event = heap.alloc_table()?;
            for (i, (name, value)) in [("target", target), ("value", value)]
                .into_iter()
                .enumerate()
            {
                let key = heap.alloc_string(name)?;
                heap.table_store(event, i as u32, key, value)?;
            }
            self.start_callback(session, change.node, closure, event)?;
        }
        Ok(true)
    }

    fn start_callback(
        &mut self,
        session: &mut Session,
        node: NodeId,
        closure: Value,
        event: Value,
    ) -> Result<bool, AdapterError> {
        self.callbacks
            .try_reserve(1)
            .map_err(|_| AdapterError::Allocation)?;
        let owner = session.create_owner()?;
        let task = match self.vm.start_closure(
            closure,
            &[event],
            RootOwner(owner.owner_id().0),
            Context::Ordinary,
        ) {
            Ok(task) => task,
            Err(error) => {
                session.revoke_owner(owner)?;
                return Err(error.into());
            }
        };
        self.callbacks.push(Callback {
            task,
            owner,
            node,
            wait: None,
            cancelling: false,
        });
        Ok(true)
    }

    pub fn has_runnable_tasks(&self) -> bool {
        if self.revoked || self.delivery.is_some() {
            return false;
        }
        if self.derived_task.is_some() || self.vm.heap().derived_pending() {
            return true;
        }
        self.issued.is_some()
            || self.task.is_some()
                && self
                    .main_wait
                    .is_none_or(|wait| !self.vm.wait_is_pending(wait))
            || self.callbacks.iter().any(|callback| {
                callback.cancelling
                    || self.stage.scene.node(callback.node).is_none()
                    || callback
                        .wait
                        .is_none_or(|wait| !self.vm.wait_is_pending(wait))
            })
    }

    pub fn has_live_work(&self) -> bool {
        if self.derived_task.is_some() || self.vm.heap().derived_pending() {
            return true;
        }
        !self.callbacks.is_empty()
            || self.activities.has_active()
            || self.audio.active()
            || self.stage.has_handlers()
    }

    pub(super) fn callback_owner(&self, session: &Session, task: TaskId) -> Option<RootOwner> {
        self.callbacks
            .iter()
            .find(|c| {
                c.task == task
                    && !c.cancelling
                    && session.is_owner_live(c.owner)
                    && self.stage.scene.node(c.node).is_some()
            })
            .map(|c| RootOwner(c.owner.owner_id().0))
    }
    pub(super) fn default_activity_owner(&self, task: TaskId) -> Result<Value, AdapterError> {
        match self.callbacks.iter().find(|c| c.task == task) {
            Some(callback) => self.stage.value(self.vm.heap(), callback.node),
            None => Ok(Value::NULL),
        }
    }

    pub(super) fn cancel_callback(
        &mut self,
        session: &mut Session,
        index: usize,
    ) -> Result<(), AdapterError> {
        let callback = &mut self.callbacks[index];
        if !callback.cancelling {
            // Current Reel natives retain no independent roots under callback
            // owners. Node/activity roots belong to the session; VM ranges are
            // released by actual task cleanup before this owner can be reused.
            session.revoke_owner_with_retirement(
                callback.owner,
                OwnerRetirementRequirements {
                    tasks_pending: true,
                    ..Default::default()
                },
            )?;
            self.activities
                .cancel_native(self.vm.heap_mut(), callback.task)?;
            self.vm.cancel_task(callback.task)?;
            callback.cancelling = true;
            callback.wait = None;
            if self
                .issued
                .is_some_and(|ticket| ticket.task == callback.task)
            {
                self.issued = None;
            }
        }
        Ok(())
    }
    pub(super) fn retire_callback(
        &mut self,
        session: &mut Session,
        index: usize,
    ) -> Result<(), AdapterError> {
        let callback = self.callbacks.remove(index);
        if callback.cancelling {
            session.confirm_owner_task_cleanup(callback.owner)?;
        } else {
            session.revoke_owner(callback.owner)?;
        }
        Ok(())
    }

    pub(super) fn step_tasks(
        &mut self,
        session: &mut Session,
        budget: SliceBudget,
        now: impl FnMut() -> u64,
    ) -> Result<SessionStep, AdapterError> {
        self.live(session)?;
        self.activities.set_skipping(session.reading().skipping());
        if self.reactive_ready && self.derived_task.is_none() {
            self.sync_narrative(session)?;
        }
        self.deliver_completion(session)?;
        for index in 0..self.callbacks.len() {
            let callback = &self.callbacks[index];
            if self.stage.scene.node(callback.node).is_none()
                || !session.is_owner_live(callback.owner)
            {
                self.cancel_callback(session, index)?;
            }
        }
        self.maintain_audio()?;
        self.activities.maintain(&mut self.vm)?;
        if self.activities.pending() {
            self.activities.drain(&mut self.vm, 64)?;
            return Ok(SessionStep::Progress);
        }
        if let Some(ticket) = self.issued {
            return Ok(SessionStep::Vm(RunOutcome::HostCall(ticket)));
        }
        if self.delivery.is_some() {
            return Err(AdapterError::Input("delivery not retired"));
        }
        if self.reactive_ready
            && self.derived_task.is_none()
            && let Some(output) = self.vm.heap_mut().next_derived()?
        {
            self.derived_task = Some(self.vm.start_derived(output, self.root_owner)?);
        }
        if let Some(task) = self.derived_task {
            let outcome = self.vm.run_slice(task, budget, now)?;
            match outcome {
                RunOutcome::HostCall(ticket) => self.issued = Some(ticket),
                RunOutcome::Returned(id) => {
                    self.vm.discard_delivery(id)?;
                    self.derived_task = None;
                    self.sync_stage(session)?;
                    return Ok(SessionStep::Progress);
                }
                RunOutcome::Fault(id) => {
                    self.delivery = Some(id);
                    self.delivery_task = Some(task);
                }
                _ => {}
            }
            return Ok(SessionStep::Vm(outcome));
        }
        let count = self.callbacks.len() + 1;
        let selected = (0..count)
            .map(|offset| (self.schedule_cursor + offset) % count)
            .find(|&index| {
                if index == 0 {
                    self.task.is_some()
                        && self
                            .main_wait
                            .is_none_or(|wait| !self.vm.wait_is_pending(wait))
                } else {
                    let callback = &self.callbacks[index - 1];
                    callback.cancelling
                        || callback
                            .wait
                            .is_none_or(|wait| !self.vm.wait_is_pending(wait))
                }
            });
        let Some(index) = selected else {
            return Ok(self
                .main_wait
                .filter(|&wait| self.vm.wait_is_pending(wait))
                .map(|wait| SessionStep::Vm(RunOutcome::Waiting(wait)))
                .unwrap_or(SessionStep::Idle));
        };
        // A budget slice yields to the host, not to another script task. Keep
        // synchronous reads/writes together across VM and native continuations.
        self.schedule_cursor = index;
        let task = if index == 0 {
            self.task.unwrap()
        } else {
            self.callbacks[index - 1].task
        };
        self.feedback_only = false;
        self.reactive_ready = false;
        let outcome = match self.vm.run_slice(task, budget, now) {
            Ok(outcome) => outcome,
            Err(VmError::Idle) if index > 0 && self.callbacks[index - 1].cancelling => {
                self.schedule_cursor = index + 1;
                self.retire_callback(session, index - 1)?;
                return Ok(SessionStep::Progress);
            }
            Err(error) => return Err(error.into()),
        };
        if matches!(
            outcome,
            RunOutcome::Returned(_) | RunOutcome::Fault(_) | RunOutcome::Waiting(_)
        ) {
            self.reactive_ready = true;
            self.sync_narrative(session)?;
            self.activities.after_script(self.vm.heap())?;
        }
        match outcome {
            RunOutcome::Returned(delivery) if index > 0 => {
                self.schedule_cursor = index + 1;
                self.vm.discard_delivery(delivery)?;
                self.retire_callback(session, index - 1)?;
                return Ok(SessionStep::Progress);
            }
            RunOutcome::HostCall(ticket) => self.issued = Some(ticket),
            RunOutcome::Returned(delivery) | RunOutcome::Fault(delivery) => {
                self.schedule_cursor = index + 1;
                self.delivery = Some(delivery);
                self.delivery_task = Some(task);
            }
            RunOutcome::Waiting(wait) => {
                self.schedule_cursor = index + 1;
                if index == 0 {
                    self.main_wait = Some(wait);
                } else {
                    self.callbacks[index - 1].wait = Some(wait);
                }
            }
            _ => {}
        }
        Ok(SessionStep::Vm(outcome))
    }
}
