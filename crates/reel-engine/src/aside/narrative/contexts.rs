//! Explicit contexts retain their original models without implicit task locals.
use super::*;
use crate::reading::ReadingId;
use aside_vm::executor::FrameId;

/// The selector and the parked parent's held-skip policy switch together.
/// This borrowed, fixed-size proposal uses the existing atomic property writer.
struct EntryWrites {
    selector: Option<(PropertyId, Value)>,
    skipping: Option<Value>,
}
impl aside_vm::PropertyWrites for EntryWrites {
    fn len(&self) -> usize {
        usize::from(self.selector.is_some()) + if self.skipping.is_some() { 3 } else { 0 }
    }
    fn get(&self, index: usize) -> Option<(PropertyId, Value)> {
        if index == 0
            && let Some(selector) = self.selector
        {
            return Some(selector);
        }
        let index = index - usize::from(self.selector.is_some());
        let field = [
            ReadingField::Skipping,
            ReadingField::SkipUnread,
            ReadingField::SkipAfterChoices,
        ]
        .get(index)?;
        Some((
            PropertyId {
                host: self.skipping?,
                field: *field as u32,
            },
            Value::bool(false),
        ))
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(in crate::aside) struct Contexts {
    #[serde(skip)]
    pub(in crate::aside) frames: std::collections::VecDeque<Context>,
    selection: Option<(Value, RootToken)>,
    #[serde(skip)]
    charge: Option<aside_vm::HeapReservation>,
}
#[derive(serde::Serialize, serde::Deserialize)]
pub(in crate::aside) struct Context {
    id: ReadingId,
    pub(in crate::aside) parent: ReadingId,
    scope: FrameId,
    pub(in crate::aside) model: Option<Narrative>,
}

impl Contexts {
    pub(in crate::aside) fn restore(
        &mut self,
        map: &aside_vm::snapshot::SnapshotMap,
    ) -> Result<(), aside_vm::snapshot::SnapshotError> {
        if let Some((value, root)) = &mut self.selection {
            *value = map.value(*value)?;
            *root = map.root(*root)?;
        }
        Ok(())
    }
    pub(in crate::aside) fn restore_frame(
        &mut self,
        mut frame: Context,
        map: &aside_vm::snapshot::SnapshotMap,
        session: &Session,
        scopes: &scopes::Scopes,
        task: Option<TaskId>,
        heap: &Heap,
    ) -> Result<(), AdapterError> {
        let index = self.frames.len();
        let parent = session.parked_readings.get(index);
        let id = session
            .parked_readings
            .get(index + 1)
            .map_or(session.reading_id, |p| p.id);
        if parent.is_none_or(|p| p.id != frame.parent)
            || frame.id != id
            || !scopes
                .live
                .iter()
                .any(|s| s.frame == frame.scope && Some(s.task) == task)
            || self
                .frames
                .back()
                .is_some_and(|p| p.scope.0 >= frame.scope.0)
        {
            return Err(AdapterError::Input("invalid reading context stack"));
        }
        if let Some(model) = &mut frame.model {
            model.restore(map, session.identity())?;
        }
        results::reserve_queue(heap, &mut self.frames, &mut self.charge)?;
        self.frames.push_back(frame);
        Ok(())
    }

    pub(in crate::aside) fn validate(&self, session: &Session) -> Result<(), AdapterError> {
        if self.frames.len() != session.parked_readings.len() {
            return Err(AdapterError::Input("incomplete reading context stack"));
        }
        Ok(())
    }
}

impl SessionVm {
    pub(in crate::aside) fn sync_context_conditions(
        &mut self,
        session: &mut Session,
    ) -> Result<(), AdapterError> {
        for frame in &mut self.narrative_contexts.frames {
            if let Some(model) = &mut frame.model {
                let ready = model.condition(self.vm.heap())?;
                session.with_reading(frame.parent, |session| {
                    session.set_auto_condition(ready);
                    model.sync_reading(self.vm.heap_mut(), session)
                })??;
            }
        }
        Ok(())
    }
    pub(in crate::aside) fn begin_reading(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
    ) -> Result<(), AdapterError> {
        self.check_story_wait(session, ticket)?;
        if self.claimed.is_some() {
            return Err(AdapterError::Input(
                "reading requires a delivered story wait",
            ));
        }
        self.sync_narrative(session)?;
        results::reserve_queue(
            self.vm.heap(),
            &mut self.narrative_contexts.frames,
            &mut self.narrative_contexts.charge,
        )?;
        let parent = session.reading_id;
        let id = session.prepare_reading()?;
        let writes = EntryWrites {
            selector: self
                .narrative_contexts
                .selection
                .map(|(host, _)| (PropertyId { host, field: 0 }, Value::int(id.0 as i64))),
            skipping: self
                .narrative
                .as_ref()
                .filter(|_| session.reading().skipping())
                .map(|model| model.reading),
        };
        if aside_vm::PropertyWrites::len(&writes) != 0 {
            self.vm.heap_mut().publish_host_properties(&writes)?;
        }
        // All fallible preparation precedes moving the original data and model.
        session.push_reading(id);
        self.narrative_contexts.frames.push_back(Context {
            id,
            parent,
            scope: ticket.frame,
            model: self.narrative.take(),
        });
        // Ordinary awaits have already resumed when main executes this entry.
        self.main_wait = None;
        self.skip_input = None;
        Ok(())
    }

    pub(in crate::aside) fn end_reading(
        &mut self,
        session: &mut Session,
        scope: FrameId,
    ) -> Result<(), AdapterError> {
        if !self
            .narrative_contexts
            .frames
            .back()
            .is_some_and(|frame| frame.scope == scope)
        {
            return Ok(());
        }
        // Reserve against the child's accumulated progress before releasing roots.
        session.reserve_parent_read_progress()?;
        let frame = self.narrative_contexts.frames.pop_back().unwrap();
        session.cancel_interaction(crate::CancellationReason::OwnerRevoked);
        session.take_completion();
        let previous = session.reading_data.control;
        session.reading_data.control = crate::reading::ReadingControl::default();
        session.reading_data.control.activate_after(previous);
        if let Some(mut model) = self.narrative.take() {
            let heap = self.vm.heap_mut();
            // Disconnect the voice/scene condition before the scene's nodes retire.
            heap.retire_host(model.auto_condition)?;
            model.sync_reading(heap, session)?;
            for (host, field) in [
                (model.dialogue, DialogueField::Active as u32),
                (model.dialogue, DialogueField::Ready as u32),
                (model.dialogue, DialogueField::AdvanceRequested as u32),
                (model.choice, ChoiceField::Active as u32),
            ] {
                publish(heap, host, field, Value::bool(false))?;
            }
            // Retained public models keep their final readable values. Receiver
            // operations resolve only live contexts; direct getter derivations
            // can unsubscribe from these models when the selector changes.
            heap.release_root(model.root)?;
        }
        session.pop_reading();
        self.narrative = frame.model;
        self.skip_input = None;
        self.publish_reading_selection(session)?;
        self.sync_narrative(session)
    }

    fn publish_reading_selection(&mut self, session: &Session) -> Result<(), AdapterError> {
        if let Some((selection, _)) = self.narrative_contexts.selection {
            publish(
                self.vm.heap_mut(),
                selection,
                0,
                Value::int(session.reading_id.0 as i64),
            )?;
        }
        Ok(())
    }

    pub(in crate::aside) fn observe_reading_selection(
        &mut self,
        session: &Session,
    ) -> Result<(), AdapterError> {
        if !self.vm.heap().reactive_evaluating() {
            return Ok(());
        }
        if self.narrative_contexts.selection.is_none() {
            let heap = self.vm.heap_mut();
            let value = record(
                heap,
                &[("reading", Value::int(session.reading_id.0 as i64))],
                &[],
            )?;
            let root = heap.add_root(self.root_owner, RootKind::Host, value)?;
            self.narrative_contexts.selection = Some((value, root));
        }
        let selection = self.narrative_contexts.selection.unwrap().0;
        self.vm.heap_mut().observe_read(selection, Some(0))?;
        Ok(())
    }

    pub(in crate::aside) fn narrative_id(
        &self,
        session: &Session,
        receiver: Value,
    ) -> Result<ReadingId, AdapterError> {
        self.vm.heap().host_fields(receiver)?;
        let contains = |n: &Narrative| {
            [n.reading, n.dialogue, n.choice]
                .iter()
                .any(|v| v.same_bits(receiver))
        };
        if self.narrative.as_ref().is_some_and(contains) {
            return Ok(session.reading_id);
        }
        self.narrative_contexts
            .frames
            .iter()
            .find(|frame| frame.model.as_ref().is_some_and(contains))
            .map(|frame| frame.parent)
            .ok_or(AdapterError::Input("retired or foreign reading context"))
    }

    pub(in crate::aside) fn take_narrative(
        &mut self,
        session: &Session,
        id: ReadingId,
    ) -> Option<Narrative> {
        if session.reading_id == id {
            self.narrative.take()
        } else {
            self.narrative_contexts
                .frames
                .iter_mut()
                .find(|f| f.parent == id)
                .unwrap()
                .model
                .take()
        }
    }
    pub(in crate::aside) fn put_narrative(
        &mut self,
        session: &Session,
        id: ReadingId,
        model: Narrative,
    ) {
        if session.reading_id == id {
            self.narrative = Some(model);
        } else {
            self.narrative_contexts
                .frames
                .iter_mut()
                .find(|f| f.parent == id)
                .unwrap()
                .model = Some(model);
        }
    }

    pub(in crate::aside) fn sync_all_reading_controls(
        &mut self,
        session: &mut Session,
    ) -> Result<(), AdapterError> {
        if let Some(model) = &mut self.narrative {
            model.sync_reading(self.vm.heap_mut(), session)?;
        }
        for frame in &mut self.narrative_contexts.frames {
            if let Some(model) = &mut frame.model {
                session.with_reading(frame.parent, |session| {
                    model.sync_reading(self.vm.heap_mut(), session)
                })??;
            }
        }
        Ok(())
    }

    pub(in crate::aside) fn sync_parked_narratives(
        &mut self,
        session: &mut Session,
    ) -> Result<(), AdapterError> {
        for frame in &mut self.narrative_contexts.frames {
            if let Some(model) = &mut frame.model {
                session.with_reading(frame.parent, |session| {
                    model.sync(self.vm.heap_mut(), session, self.read_limit)
                })??;
            }
        }
        Ok(())
    }
}
