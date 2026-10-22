//! Only unresolved editor work enters this charged, node-owned intent queue.
use super::*;
use aside_vm::{HeapReservation, executor::TaskId};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    result::{RejectCode, RejectReason},
};
use reel_ui::widgets::{
    TextInputEditingLimits, TextInputIntent, TextInputReplay, TextInputReplayId,
};
use std::collections::VecDeque;

pub(in crate::aside) struct Queue {
    pub active: Option<TaskId>,
    pub active_replay: Option<TextInputReplayId>,
    pub original_revision: u64,
    pub next_sequence: u64,
    pub entries: VecDeque<Arc<TextInputReplay>>,
    pub limits: TextInputEditingLimits,
    pub restore_expected: usize,
    text_bytes: usize,
    pub(super) pool: BudgetPool,
    charge: HeapReservation,
}

impl Queue {
    pub fn new(
        heap: &Heap,
        limits: TextInputEditingLimits,
        revision: u64,
    ) -> Result<Box<Self>, AdapterError> {
        let charge = heap.budget_usage().reserve(size_of::<Self>() as u64)?;
        Ok(Box::new(Self {
            active: None,
            active_replay: None,
            original_revision: revision,
            next_sequence: 1,
            entries: VecDeque::new(),
            limits,
            restore_expected: 0,
            text_bytes: 0,
            pool: Self::pool(limits),
            charge,
        }))
    }

    fn pool(limits: TextInputEditingLimits) -> BudgetPool {
        let overhead = (size_of::<TextInputIntent>()
            + size_of::<TextInputReplay>()
            + size_of::<reel_core::budget::BudgetTicket>()
            + size_of::<[usize; 8]>()) as u64;
        BudgetPool::new(
            BudgetCategory::CpuInput,
            QueueLimits {
                max_requests: u32::try_from(limits.max_events.saturating_mul(2))
                    .unwrap_or(u32::MAX),
                max_bytes: (limits.max_text_bytes as u64).saturating_add(
                    overhead.saturating_mul((limits.max_events as u64).saturating_mul(2)),
                ),
            },
        )
    }
    fn configure(&mut self, limits: TextInputEditingLimits) {
        if self.limits != limits && !self.pending() {
            self.pool = Self::pool(limits);
            self.limits = limits;
        }
    }

    pub fn pending(&self) -> bool {
        self.active.is_some() || !self.entries.is_empty()
    }

    pub fn push(&mut self, intent: Arc<TextInputIntent>) -> Result<(), AdapterError> {
        let text_bytes = self.text_bytes;
        if self.entries.len() >= self.limits.max_events
            || intent.text_bytes() > self.limits.max_text_bytes.saturating_sub(text_bytes)
        {
            return Err(AdapterError::InputQueue(RejectReason::new(
                RejectCode::Busy,
                "asynchronous text editing queue is full",
            )));
        }
        let bytes = intent.text_bytes()
            + size_of::<TextInputIntent>()
            + size_of::<TextInputReplay>()
            + size_of::<reel_core::budget::BudgetTicket>()
            + size_of::<[usize; 8]>();
        let ticket = self
            .pool
            .reserve(bytes as u64)
            .map_err(AdapterError::InputQueue)?;
        if self.entries.len() == self.entries.capacity() {
            let capacity = (self.entries.capacity() * 2)
                .max(4)
                .min(self.limits.max_events);
            self.charge.resize(
                (size_of::<Self>() + capacity * size_of::<Arc<TextInputReplay>>()) as u64,
            )?;
            self.entries
                .try_reserve_exact(capacity - self.entries.len())
                .map_err(|_| AdapterError::Allocation)?;
            self.charge.resize(
                (size_of::<Self>() + self.entries.capacity() * size_of::<Arc<TextInputReplay>>())
                    as u64,
            )?;
        }
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(HeapError::IdentityExhausted)?;
        self.text_bytes += intent.text_bytes();
        self.entries.push_back(Arc::new(TextInputReplay::new(
            TextInputReplayId {
                generation: self.original_revision,
                sequence,
            },
            intent,
            Arc::new(ticket),
        )));
        Ok(())
    }

    pub fn complete(&mut self, id: TextInputReplayId) -> bool {
        if self.entries.front().is_some_and(|e| e.id == id) {
            let entry = self.entries.pop_front().unwrap();
            self.text_bytes -= entry.intent.text_bytes();
            true
        } else {
            false
        }
    }

    pub fn cancel_composition(&mut self) -> bool {
        let count = self.entries.len();
        self.entries.retain(|entry| {
            let keep = !matches!(entry.intent.as_ref(), TextInputIntent::Preedit { .. });
            if !keep {
                self.text_bytes -= entry.intent.text_bytes();
            }
            keep
        });
        count != self.entries.len()
    }

    /// Explicit byte selections refer to the text actually shown when they were accepted.
    /// Relative keyboard intentions remain unresolved and use the post-edit cursor.
    pub fn prepare_remap(
        &self,
        heap: &Heap,
        start: usize,
        old: usize,
        new: usize,
    ) -> Result<Remap, AdapterError> {
        let count = self
            .entries
            .iter()
            .filter(|e| matches!(e.intent.as_ref(), TextInputIntent::Selection { .. }))
            .count();
        let charge = heap
            .budget_usage()
            .reserve((count * size_of::<(usize, Arc<TextInputReplay>)>()) as u64)?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(count)
            .map_err(|_| AdapterError::Allocation)?;
        let map = |offset: usize| {
            if offset <= start {
                offset
            } else if offset >= start + old {
                offset - old + new
            } else {
                start + (offset - start).min(new)
            }
        };
        for (index, entry) in self.entries.iter().enumerate() {
            if let TextInputIntent::Selection { selection } = entry.intent.as_ref() {
                let selection = TextSelection {
                    anchor: map(selection.anchor),
                    focus: map(selection.focus),
                };
                let bytes = size_of::<TextInputIntent>()
                    + size_of::<TextInputReplay>()
                    + size_of::<reel_core::budget::BudgetTicket>()
                    + size_of::<[usize; 8]>();
                let ticket = self
                    .pool
                    .reserve(bytes as u64)
                    .map_err(AdapterError::InputQueue)?;
                entries.push((
                    index,
                    Arc::new(TextInputReplay::new(
                        entry.id,
                        Arc::new(TextInputIntent::Selection { selection }),
                        Arc::new(ticket),
                    )),
                ));
            }
        }
        Ok(Remap {
            entries,
            _charge: charge,
        })
    }
    pub fn commit_remap(&mut self, remap: Remap) {
        for (index, entry) in remap.entries {
            self.entries[index] = entry;
        }
    }
}

pub(in crate::aside) struct Remap {
    entries: Vec<(usize, Arc<TextInputReplay>)>,
    _charge: HeapReservation,
}

pub(in crate::aside::stage) fn project(history: Option<&History>, kind: &mut NodeKind) {
    if let NodeKind::TextInput { options, .. } = kind {
        let queue = history.and_then(|h| h.queue.as_ref());
        options.pending_edit = queue.is_some_and(|q| q.pending());
        options.replay = queue
            .filter(|q| q.active.is_none())
            .and_then(|q| q.entries.front().cloned());
    }
}

impl StageBinding {
    pub(super) fn project_restored_text_queue(
        &mut self,
        node: reel_ui::stage::NodeId,
    ) -> Result<(), AdapterError> {
        let item = self.scene.node(node).unwrap();
        let mut kind = item.kind.clone();
        project(self.handles[&node].text_history.as_deref(), &mut kind);
        self.scene.update(node, kind, item.props.clone())?;
        Ok(())
    }
    pub(in crate::aside) fn mark_text_projection(&mut self, node: reel_ui::stage::NodeId) {
        if let Some(binding) = self.handles.get_mut(&node) {
            binding.projection_dirty = true;
        }
    }
    pub(in crate::aside) fn ensure_text_queue(
        &mut self,
        heap: &Heap,
        node: reel_ui::stage::NodeId,
    ) -> Result<&mut Queue, AdapterError> {
        let host = self.value(heap, node)?;
        let current = heap.host_property(PropertyId { host, field: 0 })?;
        let binding = self.handles.get_mut(&node).unwrap();
        if binding.text_history.is_none() {
            binding.text_history = Some(History::new(heap, heap.handle(current)?)?);
        }
        let history = binding.text_history.as_mut().unwrap();
        if history.queue.is_none() {
            history.queue = Some(Queue::new(
                heap,
                self.editing_limits,
                revision(heap, host)?,
            )?);
        }
        let queue = history.queue.as_deref_mut().unwrap();
        queue.configure(self.editing_limits);
        Ok(queue)
    }

    pub(in crate::aside) fn text_replay_current(
        &self,
        node: reel_ui::stage::NodeId,
        id: TextInputReplayId,
    ) -> bool {
        self.handles
            .get(&node)
            .and_then(|b| b.text_history.as_ref())
            .and_then(|h| h.queue.as_ref())
            .and_then(|q| q.entries.front())
            .is_some_and(|entry| entry.id == id)
    }

    pub(in crate::aside) fn complete_text_replay(
        &mut self,
        node: reel_ui::stage::NodeId,
        id: TextInputReplayId,
    ) {
        if let Some(binding) = self.handles.get_mut(&node)
            && let Some(queue) = binding.text_history.as_mut().and_then(|h| h.queue.as_mut())
        {
            if queue.active.is_some() && queue.active_replay == Some(id) {
                return;
            }
            binding.projection_dirty |= queue.complete(id);
        }
    }

    pub(in crate::aside) fn cancel_text_composition(&mut self, node: reel_ui::stage::NodeId) {
        if let Some(binding) = self.handles.get_mut(&node)
            && let Some(queue) = binding.text_history.as_mut().and_then(|h| h.queue.as_mut())
        {
            binding.projection_dirty |= queue.cancel_composition();
        }
    }

    pub(in crate::aside) fn retire_text_operation(
        &mut self,
        node: reel_ui::stage::NodeId,
        task: TaskId,
    ) {
        if let Some(binding) = self.handles.get_mut(&node)
            && let Some(queue) = binding.text_history.as_mut().and_then(|h| h.queue.as_mut())
            && queue.active == Some(task)
        {
            queue.active = None;
            if let Some(sequence) = queue.active_replay.take() {
                queue.complete(sequence);
            }
            binding.projection_dirty = true;
        }
    }

    pub(in crate::aside) fn finish_text_operation(
        &mut self,
        heap: &mut Heap,
        node: reel_ui::stage::NodeId,
        task: TaskId,
        remap: Option<Remap>,
    ) -> Result<(), AdapterError> {
        let host = self.value(heap, node)?;
        let binding = self.handles.get_mut(&node).unwrap();
        if let Some(queue) = binding.text_history.as_mut().and_then(|h| h.queue.as_mut())
            && queue.active == Some(task)
        {
            if let Some(remap) = remap {
                queue.commit_remap(remap);
            }
            queue.active = None;
            if let Some(sequence) = queue.active_replay.take() {
                queue.complete(sequence);
            }
        }
        let revision = revision(heap, host)?.wrapping_add(1);
        heap.set_host_payload(host, 4, Value::int(revision as i64))?;
        binding.projection_dirty = true;
        Ok(())
    }
}
