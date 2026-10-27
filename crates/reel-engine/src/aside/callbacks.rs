//! Events and spawned work share one cooperative scheduler and owner retirement.
use super::*;
use aside_vm::ValueTag;
use reel_ui::stage::NodeId;

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct ScriptTask {
    pub task: TaskId,
    pub owner: OwnerHandle,
    pub node: Option<NodeId>,
    pub result: Option<RootToken>,
    pub wait: Option<WaitId>,
    pub cancelling: bool,
    #[serde(skip)]
    pub minimum: SliceMinimum,
    // The not-yet-run VM argument roots this event. Once execution starts its
    // coordinates are immutable, even if the callback later awaits. A zero-argument
    // handler retains only the coalescing marker, with no event value.
    #[serde(skip)]
    pending_move: Option<(Option<Value>, Value)>,
}
impl ScriptTask {
    fn lifetime_live(&self, heap: &aside_vm::Heap, stage: &stage::StageBinding) -> bool {
        if let Some(root) = self.result {
            let result = heap.root_value(root).expect("task result root");
            scopes::owner_live(heap, results::owner(heap, result).expect("task owner"))
        } else {
            self.node
                .is_none_or(|node| stage.scene.node(node).is_some())
        }
    }
}

impl SessionVm {
    pub fn input_wheel(
        &mut self,
        session: &mut Session,
        input: &reel_ui::stage::NodeWheel,
    ) -> Result<bool, AdapterError> {
        self.live(session)?;
        let Some((target, callback)) = self.stage.input_wheel(self.vm.heap(), input)? else {
            return Ok(false);
        };
        self.start_callback_with_event(session, input.node, callback, |heap| {
            super::observable::record(
                heap,
                &[
                    ("target", target),
                    ("x", Value::float(input.position.x)),
                    ("y", Value::float(input.position.y)),
                    ("delta_x", Value::float(input.delta.x)),
                    ("delta_y", Value::float(input.delta.y)),
                ],
                &[],
            )
        })
    }
    pub fn input_pointer(
        &mut self,
        session: &mut Session,
        input: &reel_ui::stage::NodePointer,
    ) -> Result<bool, AdapterError> {
        self.live(session)?;
        let Some((target, callback)) = self.stage.input_pointer(self.vm.heap(), input)? else {
            return Ok(false);
        };
        use reel_ui::stage::PointerPhase;
        if input.phase == PointerPhase::Moved
            && let Some(pending) = self.tasks.last()
            && pending.node == Some(input.node)
            && !pending.cancelling
            && let Some((event, handler)) = pending.pending_move
            && handler.same_bits(callback)
        {
            if let Some(event) = event {
                let heap = self.vm.heap_mut();
                super::observable::publish(heap, event, 1, Value::float(input.position.x))?;
                super::observable::publish(heap, event, 2, Value::float(input.position.y))?;
                super::observable::publish(heap, event, 4, Value::bool(input.dragging))?;
            }
            return Ok(true);
        }
        let phase = match input.phase {
            PointerPhase::Pressed => "pressed",
            PointerPhase::Moved => "moved",
            PointerPhase::Released => "released",
            PointerPhase::Cancelled => "cancelled",
        };
        let mut argument = None;
        self.start_callback_with_event(session, input.node, callback, |heap| {
            let phase = heap.alloc_string(phase)?;
            let event = super::observable::record(
                heap,
                &[
                    ("target", target),
                    ("x", Value::float(input.position.x)),
                    ("y", Value::float(input.position.y)),
                    ("phase", phase),
                    ("dragging", Value::bool(input.dragging)),
                ],
                &[],
            )?;
            argument = Some(event);
            Ok(event)
        })?;
        if input.phase == PointerPhase::Moved {
            self.tasks.last_mut().unwrap().pending_move = Some((argument, callback));
        }
        Ok(true)
    }
    pub fn input_key(
        &mut self,
        session: &mut Session,
        input: &reel_ui::stage::NodeKey,
    ) -> Result<bool, AdapterError> {
        self.live(session)?;
        let Some((target, callback)) = self.stage.input_key(self.vm.heap(), input)? else {
            return Ok(false);
        };
        use reel_ui::input::ActionPhase;
        let phase = match input.phase {
            ActionPhase::Pressed => "pressed",
            ActionPhase::Repeated => "repeated",
            ActionPhase::Released => "released",
            ActionPhase::Cancelled => "cancelled",
            ActionPhase::Changed => return Err(AdapterError::Input("invalid key phase")),
        };
        self.start_callback_with_event(session, input.node, callback, |heap| {
            let event = heap.alloc_table()?;
            let code = heap.alloc_string(&input.event.code.to_string())?;
            let key = heap.alloc_string(&input.event.key.to_string())?;
            let phase = heap.alloc_string(phase)?;
            let mods = input.event.modifiers;
            for (i, (name, value)) in [
                ("target", target),
                ("code", code),
                ("key", key),
                ("phase", phase),
                ("shift", Value::bool(mods.shift())),
                ("ctrl", Value::bool(mods.ctrl())),
                ("alt", Value::bool(mods.alt())),
                ("meta", Value::bool(mods.meta())),
            ]
            .into_iter()
            .enumerate()
            {
                let key = heap.alloc_string(name)?;
                heap.table_store(event, i as u32, key, value)?;
            }
            Ok(event)
        })
    }
    /// Publish input feedback before dispatching callbacks. A batch uses one stage sync.
    pub fn interactions(
        &mut self,
        session: &mut Session,
        events: &[reel_ui::stage::NodeEvent],
    ) -> Result<bool, AdapterError> {
        self.live(session)?;
        let pending = self.vm.heap().derived_pending();
        let mut changed = false;
        let mut callbacks = Vec::new();
        for event in events {
            match event {
                reel_ui::stage::NodeEvent::Interaction(event) => {
                    if self.stage.interaction(self.vm.heap_mut(), event)? {
                        changed = true;
                        if let Some((target, callback)) = self
                            .stage
                            .interaction_callback(self.vm.heap(), event.node)?
                        {
                            callbacks
                                .try_reserve(1)
                                .map_err(|_| AdapterError::Allocation)?;
                            callbacks.push((event.node, target, callback, event.state));
                        }
                    }
                }
                reel_ui::stage::NodeEvent::Scroll(event) => {
                    if event.input {
                        self.virtual_scroll_input(event.node)?;
                    }
                    let (updated, accepted_y) = self.stage.scroll(self.vm.heap_mut(), event)?;
                    changed |= updated;
                    if !event.input && accepted_y {
                        self.virtual_scroll_layout(event)?;
                    }
                }
                reel_ui::stage::NodeEvent::VirtualList(event) => {
                    changed |= self.virtual_feedback(event)?;
                }
                _ => {}
            }
        }
        if changed {
            self.feedback_only = self.reactive_ready && !pending && self.derived_task.is_none();
            self.sync_stage_properties(self.feedback_only)?;
        }
        // Each callback sees the complete batch's current node state and its
        // own immutable transition. No events or tasks are made without a handler.
        for (node, target, callback, state) in callbacks {
            self.start_callback_with_event(session, node, callback, |heap| {
                super::observable::record(
                    heap,
                    &[
                        ("target", target),
                        ("hover", Value::bool(state.hover)),
                        ("pressed", Value::bool(state.pressed)),
                        ("focused", Value::bool(state.focused)),
                        ("disabled", Value::bool(state.disabled)),
                        ("focus_visible", Value::bool(state.focus_visible)),
                    ],
                    &[],
                )
            })?;
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
        self.start_callback_with_event(session, node, closure, |heap| {
            let event = heap.alloc_table()?;
            let key = heap.alloc_string("target")?;
            heap.table_store(event, 0, key, target)?;
            Ok(event)
        })
    }

    /// Route a presented popup's close request through its normal node lifetime.
    pub fn dismiss_popup(
        &mut self,
        session: &mut Session,
        input: &reel_ui::stage::NodePopupDismiss,
    ) -> Result<bool, AdapterError> {
        self.live(session)?;
        let Some((target, callback)) = self.stage.dismiss_popup(self.vm.heap(), input.node)? else {
            return Ok(false);
        };
        self.start_callback_with_event(session, input.node, callback, |heap| {
            let reason = heap.alloc_string(match input.reason {
                reel_ui::widgets::PopupDismiss::Outside => "outside",
                reel_ui::widgets::PopupDismiss::Back => "back",
            })?;
            super::observable::record(heap, &[("target", target), ("reason", reason)], &[])
        })
    }

    pub fn text_link(
        &mut self,
        session: &mut Session,
        link: &reel_ui::stage::NodeLink,
    ) -> Result<bool, AdapterError> {
        self.live(session)?;
        let Some((target, callback)) = self.stage.text_link(self.vm.heap(), link)? else {
            return Ok(false);
        };
        let read_limit = self.read_limit;
        self.start_callback_with_event(session, link.node, callback, |heap| {
            let id = super::text::alloc_string(heap, &link.id, read_limit)?;
            super::observable::record(heap, &[("target", target), ("id", id)], &[])
        })
    }

    pub fn input_action(
        &mut self,
        session: &mut Session,
        action: &reel_ui::stage::NodeAction,
    ) -> Result<bool, AdapterError> {
        self.live(session)?;
        let Some(selected) = self.stage.input_action(self.vm.heap_mut(), action)? else {
            return Ok(false);
        };
        use reel_ui::input::ActionPhase;
        let phase = match selected.phase {
            ActionPhase::Pressed => "pressed",
            ActionPhase::Changed => "changed",
            ActionPhase::Repeated => "repeated",
            ActionPhase::Released => "released",
            ActionPhase::Cancelled => "cancelled",
        };
        self.start_callback_with_event(session, action.node, selected.closure, |heap| {
            let event = heap.alloc_table()?;
            let name = heap.alloc_string(&action.action)?;
            let phase = heap.alloc_string(phase)?;
            for (i, (name, value)) in [
                ("target", selected.target),
                ("action", name),
                ("value", Value::float(selected.value.into())),
                ("phase", phase),
            ]
            .into_iter()
            .enumerate()
            {
                let key = heap.alloc_string(name)?;
                heap.table_store(event, i as u32, key, value)?;
            }
            Ok(event)
        })
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
        self.sync_stage_properties(false)?;
        if closure.tag() != ValueTag::Null {
            self.start_callback_with_event(session, change.node, closure, |heap| {
                let event = heap.alloc_table()?;
                for (i, (name, value)) in [("target", target), ("value", value)]
                    .into_iter()
                    .enumerate()
                {
                    let key = heap.alloc_string(name)?;
                    heap.table_store(event, i as u32, key, value)?;
                }
                Ok(event)
            })?;
        }
        Ok(true)
    }

    /// Composition stays in the widget; only committed values enter script state.
    pub fn text_input(
        &mut self,
        session: &mut Session,
        input: &reel_ui::stage::NodeText,
    ) -> Result<bool, AdapterError> {
        self.live(session)?;
        if let Some(id) = input.event.replay_id()
            && !self.stage.text_replay_current(input.node, id)
        {
            return Ok(false);
        }
        if matches!(
            input.event,
            reel_ui::widgets::TextInputEvent::CompositionCancelled
        ) {
            self.stage.cancel_text_composition(input.node);
            self.sync_stage_properties(false)?;
            return Ok(true);
        }
        if let reel_ui::widgets::TextInputEvent::Deferred { intent, revision } = &input.event {
            if !self.stage.text_target_current(self.vm.heap(), input.node)? {
                return Ok(false);
            }
            let host = self.stage.value(self.vm.heap(), input.node)?;
            let current_revision = self.vm.heap().host_payload(host, 4)?.as_int().unwrap() as u64;
            if matches!(
                intent.as_ref(),
                reel_ui::widgets::TextInputIntent::Selection { .. }
            ) && *revision != current_revision
            {
                return Ok(false);
            }
            let queue = self.stage.ensure_text_queue(self.vm.heap(), input.node)?;
            if *revision < queue.original_revision || *revision > current_revision {
                return Err(AdapterError::Input(
                    "editing intent belongs to a replaced selection revision",
                ));
            }
            queue.push(intent.clone())?;
            self.stage.mark_text_projection(input.node);
            self.sync_stage_properties(false)?;
            return Ok(true);
        }
        if let reel_ui::widgets::TextInputEvent::ReplayCompleted(sequence) = input.event {
            self.stage.complete_text_replay(input.node, sequence);
            self.sync_stage_properties(false)?;
            return Ok(true);
        }
        if let reel_ui::widgets::TextInputEvent::Undo {
            value,
            revision,
            replay,
        }
        | reel_ui::widgets::TextInputEvent::Redo {
            value,
            revision,
            replay,
        } = &input.event
        {
            if !self
                .stage
                .text_action(self.vm.heap(), input.node, value, *revision)?
            {
                return Ok(false);
            }
            let target = self.stage.value(self.vm.heap(), input.node)?;
            let name = if matches!(input.event, reel_ui::widgets::TextInputEvent::Redo { .. }) {
                "redo"
            } else {
                "undo"
            };
            let method = self
                .vm
                .bind_host_method(target, name)?
                .expect("registered editor method");
            self.stage.ensure_text_queue(self.vm.heap(), input.node)?;
            let task = self.start_owned_task(session, Some(input.node), method, &[], None)?;
            let queue = self.stage.ensure_text_queue(self.vm.heap(), input.node)?;
            queue.active = Some(task);
            queue.active_replay = *replay;
            self.stage.mark_text_projection(input.node);
            self.sync_stage_properties(false)?;
            return Ok(true);
        }
        if let Some(accepted) = self.start_filtered_edit(session, input)? {
            return Ok(accepted);
        }
        let Some((target, closure, value)) = self.stage.text_input(self.vm.heap_mut(), input)?
        else {
            return Ok(false);
        };
        self.sync_stage_properties(false)?;
        self.start_text_callback(session, input.node, target, closure, value)?;
        Ok(true)
    }

    pub(super) fn start_text_callback(
        &mut self,
        session: &mut Session,
        node: NodeId,
        target: Value,
        closure: Value,
        value: Value,
    ) -> Result<(), AdapterError> {
        if closure.tag() != ValueTag::Null {
            self.start_callback_with_event(session, node, closure, |heap| {
                let event = heap.alloc_table()?;
                for (i, (name, value)) in [("target", target), ("value", value)]
                    .into_iter()
                    .enumerate()
                {
                    let key = heap.alloc_string(name)?;
                    heap.table_store(event, i as u32, key, value)?;
                }
                Ok(event)
            })?;
        }
        Ok(())
    }

    // Build the input payload only when the callable accepts an argument.
    // Task creation and node ownership follow the same path for every handler.
    fn start_callback_with_event(
        &mut self,
        session: &mut Session,
        node: NodeId,
        closure: Value,
        event: impl FnOnce(&mut aside_vm::Heap) -> Result<Value, AdapterError>,
    ) -> Result<bool, AdapterError> {
        let argument = if self.vm.callable_has_parameters(closure)? {
            Some(event(self.vm.heap_mut())?)
        } else {
            None
        };
        self.start_owned_task(session, Some(node), closure, argument.as_slice(), None)?;
        Ok(true)
    }

    pub(super) fn start_owned_task(
        &mut self,
        session: &mut Session,
        node: Option<NodeId>,
        closure: Value,
        arguments: &[Value],
        result: Option<Value>,
    ) -> Result<TaskId, AdapterError> {
        self.start_owned_task_in_context(
            session,
            node,
            closure,
            arguments,
            result,
            Context::Ordinary,
        )
    }

    pub(super) fn start_owned_task_in_context(
        &mut self,
        session: &mut Session,
        node: Option<NodeId>,
        closure: Value,
        arguments: &[Value],
        result: Option<Value>,
        context: Context,
    ) -> Result<TaskId, AdapterError> {
        self.tasks
            .try_reserve(1)
            .map_err(|_| AdapterError::Allocation)?;
        let owner = session.create_owner()?;
        let result = match result {
            Some(value) => match self.vm.heap_mut().add_root(
                RootOwner(owner.owner_id().0),
                RootKind::Host,
                value,
            ) {
                Ok(root) => Some(root),
                Err(error) => {
                    session.revoke_owner(owner)?;
                    return Err(error.into());
                }
            },
            None => None,
        };
        let task =
            match self
                .vm
                .start_closure(closure, arguments, RootOwner(owner.owner_id().0), context)
            {
                Ok(task) => task,
                Err(error) => {
                    if let Some(root) = result {
                        self.vm.heap_mut().release_root(root)?;
                    }
                    session.revoke_owner(owner)?;
                    return Err(error.into());
                }
            };
        self.tasks.push(ScriptTask {
            task,
            owner,
            node,
            result,
            wait: None,
            cancelling: false,
            minimum: SliceMinimum::default(),
            pending_move: None,
        });
        self.checked_callback_lifetimes = None;
        self.blocked_tasks_revision = None;
        Ok(task)
    }

    pub fn has_runnable_tasks(&self) -> bool {
        if self.revoked || self.exit_requested || self.delivery.is_some() {
            return false;
        }
        if self.derived_task.is_some() || self.vm.heap().derived_pending() {
            return true;
        }
        self.scopes_pending()
            || self.transitions_pending()
            || self.transitions.pending(self.activities.time())
            || self.particles.needs_work(self.stage.retirement_revision)
            || self
                .results
                .needs_work(self.stage.scene.revision(), self.stage.retirement_revision)
            || self.issued.is_some()
            || !self.tasks.is_empty()
                && self
                    .checked_callback_lifetimes
                    .is_none_or(|(host, stage, _)| {
                        host != self.vm.heap().host_retirement_revision()
                            || stage != self.stage.retirement_revision
                    })
            || self.blocked_tasks_revision != Some(self.vm.wait_revision())
                && (self.task.is_some()
                    && self
                        .main_wait
                        .is_none_or(|wait| !self.vm.wait_is_pending(wait))
                    || self.tasks.iter().any(|callback| {
                        callback.cancelling
                            || callback
                                .wait
                                .is_none_or(|wait| !self.vm.wait_is_pending(wait))
                    }))
    }

    pub fn has_live_work(&self) -> bool {
        if self.derived_task.is_some() || self.vm.heap().derived_pending() {
            return true;
        }
        !self.tasks.is_empty()
            || self.activities.has_active()
            || self.particles.has_active()
            || self.media.active()
            || self.saves.active()
            || self.captures.active()
            || self.asset_reads.active()
            || self.transitions.active()
            || self.stage.has_handlers()
    }

    pub(super) fn callback_owner(&self, session: &Session, task: TaskId) -> Option<RootOwner> {
        self.tasks
            .iter()
            .find(|c| {
                c.task == task
                    && !c.cancelling
                    && session.is_owner_live(c.owner)
                    && c.lifetime_live(self.vm.heap(), &self.stage)
            })
            .map(|c| RootOwner(c.owner.owner_id().0))
    }
    pub(super) fn default_activity_owner(&self, task: TaskId) -> Result<Value, AdapterError> {
        if let Some(scope) = self.current_scope(task)? {
            return Ok(scope);
        }
        if let Some(root) = self
            .tasks
            .iter()
            .find(|c| c.task == task)
            .and_then(|c| c.result)
        {
            return results::owner(self.vm.heap(), self.vm.heap().root_value(root)?);
        }
        match self
            .tasks
            .iter()
            .find(|c| c.task == task)
            .and_then(|c| c.node)
        {
            Some(node) => self.stage.value(self.vm.heap(), node),
            None => Ok(Value::NULL),
        }
    }

    pub(super) fn cancel_callback(
        &mut self,
        session: &mut Session,
        index: usize,
    ) -> Result<(), AdapterError> {
        self.cancel_task_transitions(self.tasks[index].task, None)?;
        self.cancel_task_scopes(session, self.tasks[index].task)?;
        let callback = &mut self.tasks[index];
        if !callback.cancelling {
            callback.minimum = SliceMinimum::default();
            callback.pending_move = None;
            // Spawned result roots remain owned by this task until cleanup. Node
            // and activity roots belong to the session and their explicit nodes.
            if let Some(root) = callback.result {
                let object = self.vm.heap().root_value(root)?;
                super::results::cancelling(self.vm.heap_mut(), object)?;
            }
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
            self.results.changed();
            callback.cancelling = true;
            callback.wait = None;
            if self
                .issued
                .is_some_and(|ticket| ticket.task == callback.task)
            {
                self.issued = None;
                self.saves.capture = None;
            }
        }
        Ok(())
    }
    pub(super) fn retire_callback(
        &mut self,
        session: &mut Session,
        index: usize,
    ) -> Result<(), AdapterError> {
        let callback = self.tasks.remove(index);
        if let Some(node) = callback.node {
            self.stage.retire_text_operation(node, callback.task);
        }
        // Removing a callback shifts later scheduler slots (slot 0 is main).
        // Keep the next task's identity, including across an empty idle period.
        if self.schedule_cursor > index + 1 {
            self.schedule_cursor -= 1;
        }
        self.schedule_cursor %= self.tasks.len() + 1;
        if let Some(root) = callback.result {
            if callback.cancelling {
                let object = self.vm.heap().root_value(root)?;
                super::results::finish_task(
                    self.vm.heap_mut(),
                    object,
                    super::results::TaskResult::Cancelled,
                )?;
                self.results.changed();
            }
            self.vm.heap_mut().release_root(root)?;
        }
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
        budget: SessionBudget,
        mut now: impl FnMut() -> u64,
    ) -> Result<SessionStep, AdapterError> {
        self.live(session)?;
        if self
            .skip_input
            .is_some_and(|node| self.stage.scene.node(node).is_none())
        {
            session.set_skip_reading(false, false, false);
        }
        if !session.reading().skipping() {
            self.skip_input = None;
        }
        self.activities.set_skipping(session.reading().skipping());
        if self.reactive_ready && self.reactive_settled() {
            self.sync_auto_condition(session)?;
            self.sync_narrative(session)?;
        }
        self.deliver_completion(session)?;
        self.maintain_scopes(session)?;
        self.maintain_transitions(session)?;
        let lifetimes = (
            self.vm.heap().host_retirement_revision(),
            self.stage.retirement_revision,
            session.owner_revocation_revision(),
        );
        if self.checked_callback_lifetimes != Some(lifetimes) {
            for index in 0..self.tasks.len() {
                #[cfg(test)]
                {
                    self.callback_lifetime_checks += 1;
                }
                let callback = &self.tasks[index];
                if !callback.lifetime_live(self.vm.heap(), &self.stage)
                    || !session.is_owner_live(callback.owner)
                {
                    self.cancel_callback(session, index)?;
                }
            }
            // Cancellation may retire another owner; the next entry observes it.
            self.checked_callback_lifetimes = Some(lifetimes);
        }
        self.maintain_media()?;
        self.maintain_localization()?;
        self.maintain_asset_reads(64)?;
        self.activities.maintain(&mut self.vm)?;
        self.maintain_particles(64)?;
        if self.particles.needs_work(self.stage.retirement_revision) {
            return Ok(SessionStep::Progress);
        }
        let evaluating = self
            .derived_task
            .map(|task| self.vm.reactive_task_output(task))
            .transpose()?
            .flatten();
        if self.results.maintain(
            &mut self.vm,
            self.stage.scene.revision(),
            self.stage.retirement_revision,
            |heap, result, owner| {
                lists::virtualized::presentation_current(
                    heap,
                    &self.stage,
                    result,
                    owner,
                    evaluating,
                )
            },
        )? {
            return Ok(SessionStep::Progress);
        }
        if self.activities.pending() {
            self.activities.drain(&mut self.vm, 64)?;
            return Ok(SessionStep::Progress);
        }
        if self.transitions.pending(self.activities.time()) {
            self.transitions.advance(self.activities.time(), 64);
            return Ok(SessionStep::Progress);
        }
        if let Some(ticket) = self.issued {
            return Ok(SessionStep::Vm(RunOutcome::HostCall(ticket)));
        }
        if self.delivery.is_some() {
            return Err(AdapterError::Input("delivery not retired"));
        }
        if let Some(progress) = self.step_reactive(session, budget, &mut now)? {
            return Ok(progress);
        }
        let count = self.tasks.len() + 1;
        let selected = (self.blocked_tasks_revision != Some(self.vm.wait_revision()))
            .then(|| {
                (0..count)
                    .map(|offset| (self.schedule_cursor + offset) % count)
                    .find(|&index| {
                        if index == 0 {
                            self.task.is_some()
                                && self
                                    .main_wait
                                    .is_none_or(|wait| !self.vm.wait_is_pending(wait))
                        } else {
                            let callback = &self.tasks[index - 1];
                            callback.cancelling
                                || callback
                                    .wait
                                    .is_none_or(|wait| !self.vm.wait_is_pending(wait))
                        }
                    })
            })
            .flatten();
        let Some(index) = selected else {
            self.blocked_tasks_revision = Some(self.vm.wait_revision());
            return Ok(self
                .main_wait
                .filter(|&wait| self.vm.wait_is_pending(wait))
                .map(|wait| SessionStep::Vm(RunOutcome::Waiting(wait)))
                .unwrap_or(SessionStep::Idle));
        };
        self.blocked_tasks_revision = None;
        // A budget slice yields to the host, not to another script task. Keep
        // synchronous reads/writes together across VM and native continuations.
        self.schedule_cursor = index;
        let task = if index == 0 {
            self.task.unwrap()
        } else {
            self.tasks[index - 1].pending_move = None;
            self.tasks[index - 1].task
        };
        self.feedback_only = false;
        self.reactive_ready = false;
        let minimum = if index == 0 {
            &mut self.main_minimum
        } else {
            &mut self.tasks[index - 1].minimum
        };
        let outcome = match minimum.run(&mut self.vm, task, budget, now) {
            Ok(outcome) => outcome,
            Err(VmError::Idle) if index > 0 && self.tasks[index - 1].cancelling => {
                self.media.finish_batch(task, false);
                self.schedule_cursor = index + 1;
                self.retire_callback(session, index - 1)?;
                self.reactive_ready = true;
                self.sync_narrative(session)?;
                self.activities.after_script(self.vm.heap())?;
                return Ok(SessionStep::Progress);
            }
            Err(error) => return Err(error.into()),
        };
        let await_boundary = matches!(
            outcome,
            RunOutcome::HostCall(ticket)
                if ticket.input == NativeInput::Entry && ticket.await_activation.is_some()
        );
        if await_boundary
            || matches!(
                outcome,
                RunOutcome::Returned(_) | RunOutcome::Fault(_) | RunOutcome::Waiting(_)
            )
        {
            self.media
                .finish_batch(task, !matches!(outcome, RunOutcome::Fault(_)));
            self.reactive_ready = true;
            self.sync_narrative(session)?;
            self.activities.after_script(self.vm.heap())?;
        }
        match outcome {
            RunOutcome::Returned(delivery) | RunOutcome::Fault(delivery)
                if index > 0 && self.tasks[index - 1].result.is_some() =>
            {
                let root = self.tasks[index - 1].result.unwrap();
                let object = self.vm.heap().root_value(root)?;
                let result = match self.vm.delivery_fault(delivery)? {
                    Some(fault) => super::results::TaskResult::Faulted(fault.as_value()),
                    None => super::results::TaskResult::Returned(self.vm.delivery_value(delivery)?),
                };
                super::results::finish_task(self.vm.heap_mut(), object, result)?;
                self.results.changed();
                self.schedule_cursor = index + 1;
                self.vm.discard_delivery(delivery)?;
                self.retire_callback(session, index - 1)?;
                return Ok(SessionStep::Progress);
            }
            RunOutcome::Returned(delivery) if index > 0 => {
                self.schedule_cursor = index + 1;
                self.vm.discard_delivery(delivery)?;
                self.retire_callback(session, index - 1)?;
                return Ok(SessionStep::Progress);
            }
            RunOutcome::HostCall(ticket) => {
                self.issued = Some(ticket);
                if await_boundary {
                    self.schedule_cursor = index + 1;
                }
            }
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
                    self.tasks[index - 1].wait = Some(wait);
                }
            }
            _ => {}
        }
        Ok(SessionStep::Vm(outcome))
    }
}

impl SessionVm {
    /// Rebuild only derived presentation values; no story/callback task is run.
    pub fn step_reactive(
        &mut self,
        session: &Session,
        budget: SessionBudget,
        now: impl FnMut() -> u64,
    ) -> Result<Option<SessionStep>, AdapterError> {
        if self.reactive_ready
            && self.derived_task.is_none()
            && let Some(output) = self.vm.heap_mut().next_derived()?
        {
            self.derived_task = Some(self.vm.start_derived(output, self.root_owner)?);
            self.derived_minimum = SliceMinimum::default();
        }
        if let Some(task) = self.derived_task {
            let outcome = self.derived_minimum.run(&mut self.vm, task, budget, now)?;
            match outcome {
                RunOutcome::HostCall(ticket) => self.issued = Some(ticket),
                RunOutcome::Returned(id) => {
                    self.vm.discard_delivery(id)?;
                    self.derived_task = None;
                    self.sync_stage(session)?;
                    return Ok(Some(SessionStep::Progress));
                }
                RunOutcome::Fault(id) => {
                    self.delivery = Some(id);
                    self.delivery_task = Some(task);
                }
                _ => {}
            }
            return Ok(Some(SessionStep::Vm(outcome)));
        }
        Ok(None)
    }
}
