//! Read dependencies share heap ownership and weak reverse subscriptions.
use super::*;

mod freshness;
mod property_cycle;
mod scan;
#[cfg(test)]
mod subscription_tests;
pub(in crate::heap) use property_cycle::PropertyGraphProgress;
use scan::DependencyCursor;

#[derive(Clone, Copy)]
#[repr(u32)]
pub(crate) enum DerivedSlot {
    Closure,
    Dependencies,
    // Active captured edges, or an empty inline scratch while Evaluation is zero.
    Candidate,
    Weak,
    Parent,
    Evaluation,
    Function,
    Origin,
    CallPc,
    Arguments,
    State,
    Suspended,
    // Standard derive may replace its entry bridge with a proven expression frame.
    Compact,
}

impl Heap {
    pub fn alloc_derived(&mut self, closure: Value) -> Result<Value, HeapError> {
        let fields = self.alloc_table()?;
        let name = self.alloc_string("value")?;
        self.table_store(fields, 0, name, Value::NULL)?;
        let output = self.alloc_host(
            fields,
            &[
                closure,
                Value::NULL,
                Value::NULL,
                Value::NULL,
                Value::NULL,
                Value::NULL,
                Value::NULL,
                Value::NULL,
                Value::NULL,
                Value::NULL,
                Value::NULL,
                Value::NULL,
                Value::NULL,
            ],
            None,
        )?;
        self.observe_host_properties(output, 0, 1)?;
        let key = self.key(output)?;
        self.objects.get_mut(key).unwrap().derived = true;
        let weak = self.alloc_weakref(output)?;
        self.set_host_payload(output, DerivedSlot::Weak as u32, weak)?;
        Ok(output)
    }

    pub fn derived_state(&self, output: Value) -> Result<Value, HeapError> {
        self.host_payload(output, DerivedSlot::State as u32)
    }
    pub fn set_derived_state(&mut self, output: Value, state: Value) -> Result<(), HeapError> {
        self.set_host_payload(output, DerivedSlot::State as u32, state)
    }

    pub fn derived_closure(&self, output: Value) -> Result<Value, HeapError> {
        self.host_payload(output, DerivedSlot::Closure as u32)
    }

    /// Begin a transactional dependency capture, reusing empty inline scratch.
    /// Committed inputs remain intact while this candidate is being evaluated.
    pub fn begin_derived(&mut self, output: Value) -> Result<(), HeapError> {
        let scratch = self.host_payload(output, DerivedSlot::Candidate as u32)?;
        let candidate = if scratch.same_bits(Value::NULL) {
            self.alloc_dependencies()?
        } else {
            scratch
        };
        self.begin_capture(output, candidate)
    }
    pub(super) fn alloc_dependencies(&mut self) -> Result<Value, HeapError> {
        self.ensure_object_slot()?;
        self.ensure_bytes(crate::dependencies::BASE_BYTES)?;
        Ok(self.allocate(
            ObjectKind::Dependencies,
            Payload::Dependencies(Box::default()),
            None,
            crate::dependencies::BASE_BYTES,
        ))
    }

    /// Isolate component construction from the enclosing observer. Nested
    /// derivations still collect their own dependencies. The native frame owns
    /// `output` and must end it on return/fault/cancellation, just like derive.
    pub fn begin_untracked(&mut self, output: Value) -> Result<(), HeapError> {
        self.begin_capture(output, Value::NULL)
    }

    fn begin_capture(&mut self, output: Value, candidate: Value) -> Result<(), HeapError> {
        self.set_host_payload(output, DerivedSlot::Candidate as u32, candidate)?;
        self.set_host_payload(
            output,
            DerivedSlot::Parent as u32,
            self.reactive_capture.unwrap_or(Value::NULL),
        )?;
        self.evaluation = self.evaluation.wrapping_add(1).max(1);
        self.set_host_payload(
            output,
            DerivedSlot::Evaluation as u32,
            Value::int(self.evaluation as i64),
        )?;
        self.reactive_capture = Some(output);
        Ok(())
    }

    /// End or abandon the active capture and restore its parent. Small candidates
    /// release their strong edges into reusable empty scratch; larger candidates
    /// are detached for incremental GC. This does not replace committed inputs.
    pub fn end_derived(&mut self, output: Value) -> Result<(), HeapError> {
        if !self
            .reactive_capture
            .is_some_and(|active| active.same_bits(output))
        {
            return Ok(());
        }
        let parent = self.host_payload(output, DerivedSlot::Parent as u32)?;
        self.reactive_capture = (!parent.same_bits(Value::NULL)).then_some(parent);
        self.set_host_payload(output, DerivedSlot::Parent as u32, Value::NULL)?;
        self.set_host_payload(output, DerivedSlot::Evaluation as u32, Value::int(0))?;
        let candidate = self.host_payload(output, DerivedSlot::Candidate as u32)?;
        let reusable = if candidate.same_bits(Value::NULL) {
            false
        } else {
            let key = self.key(candidate)?;
            let Payload::Dependencies(capture) = &mut self.objects.get_mut(key).unwrap().payload
            else {
                unreachable!()
            };
            let edges = capture.values.len;
            let reusable = capture.recycle_small();
            if reusable {
                self.property_traversal_work
                    .set(self.property_traversal_work.get() + u64::from(edges));
            }
            reusable
        };
        if !reusable {
            self.set_host_payload(output, DerivedSlot::Candidate as u32, Value::NULL)?;
        }
        Ok(())
    }

    /// Objects created by this evaluation are private until it returns.
    pub fn derived_write_allowed(&self, target: Value) -> bool {
        self.reactive_capture.is_some_and(|output| {
            let generation = self
                .host_payload(output, DerivedSlot::Evaluation as u32)
                .ok()
                .and_then(Value::as_int);
            self.key(target)
                .ok()
                .and_then(|key| self.objects.get(key))
                .is_some_and(|object| {
                    generation == Some(object.evaluation as i64)
                        && !object.model
                        && object.observable_fields == 0
                })
        })
    }

    /// Called at semantic reads, never during key lookup, validation or GC.
    pub fn observe_read(&mut self, value: Value, field: Option<u32>) -> Result<(), HeapError> {
        let Some(output) = self.reactive_capture else {
            return Ok(());
        };
        let dependencies = self.host_payload(output, DerivedSlot::Candidate as u32)?;
        if dependencies.same_bits(Value::NULL) {
            return Ok(());
        }
        if value.tag() != crate::ValueTag::Object {
            return Ok(());
        }
        let object = self.object(self.key(value)?)?;
        let mut source = value;
        if let Some(owner) = object.property_owner {
            source = Value::object(self.id, owner.data().as_ffi());
        } else if !object.model && object.observable_fields == 0 {
            return Ok(());
        }
        let object = self.object(self.key(source)?)?;
        // Private construction reads do not subscribe a component to its own
        // newborn nodes. Read-only host services may be initialized lazily here;
        // their observable identity is shared and must still be tracked.
        if !object.derived
            && (object.observable_writable != 0 || object.model && !object.readonly_collection)
            && object.evaluation as i64
                == self
                    .host_payload(output, DerivedSlot::Evaluation as u32)?
                    .as_int()
                    .unwrap()
        {
            return Ok(());
        }
        if let Some(index) = field
            && index >= self.object(self.key(source)?)?.observable_fields
        {
            return Ok(());
        }
        self.capture_dependency(dependencies, source, field)?;
        Ok(())
    }
    pub(super) fn capture_dependency(
        &mut self,
        dependencies: Value,
        source: Value,
        field: Option<u32>,
    ) -> Result<bool, HeapError> {
        let selector = field.map_or(-1, i64::from);
        let key = self.key(dependencies)?;
        let Payload::Dependencies(capture) = &self.object(key)?.payload else {
            unreachable!()
        };
        if capture.contains(source, field) {
            return Ok(false);
        }
        let extra = capture.pair_bytes()?;
        let chunks = u64::from(Values::growth_bytes(capture.values.len.into(), 2) != 0);
        self.ensure_bytes(extra)?;
        self.barrier(source)?;
        self.charge(extra, chunks);
        let object = self.objects.get_mut(key).unwrap();
        let Payload::Dependencies(capture) = &mut object.payload else {
            unreachable!()
        };
        capture.push_pair(source, Value::int(selector));
        object.charged += extra;
        Ok(true)
    }

    pub fn observe_write(&self, value: Value, field: Option<u32>) -> Result<(), HeapError> {
        let Some(output) = self.reactive_capture else {
            return Ok(());
        };
        let dependencies = self.host_payload(output, DerivedSlot::Candidate as u32)?;
        if dependencies.same_bits(Value::NULL) {
            return Ok(());
        }
        let object = self.object(self.key(value)?)?;
        let source = object
            .property_owner
            .map_or(value, |owner| Value::object(self.id, owner.data().as_ffi()));
        if object.property_owner.is_none() && !object.model && object.observable_fields == 0 {
            return Ok(());
        }
        let Payload::Dependencies(capture) = &self.object(self.key(dependencies)?)?.payload else {
            unreachable!()
        };
        if capture.overlaps(source, field) {
            return Err(HeapError::BindingCycle);
        }
        Ok(())
    }

    fn enqueue_derived(&mut self, key: HeapKey) {
        if !self.objects.get(key).unwrap().derived_dirty {
            if self.reactive_queue.len() == self.reactive_queue.capacity() {
                self.reactive_queue
                    .retain(|key| self.objects.get(*key).is_some_and(|o| !o.dying));
            }
            self.objects.get_mut(key).unwrap().derived_dirty = true;
            self.reactive_queue.push_back(key);
        }
    }

    fn reactive_remove(
        &mut self,
        source: Value,
        weak: Value,
        selector: Value,
    ) -> Result<(), HeapError> {
        let subscribers = self.object(self.key(source)?)?.reactive_subscribers;
        if subscribers.same_bits(Value::NULL) {
            return Ok(());
        }
        let mut index = 0;
        while index < self.sequence_len(subscribers)? {
            self.property_traversal_work
                .set(self.property_traversal_work.get() + 8);
            if self.sequence_get(subscribers, index)?.same_bits(weak)
                && self
                    .sequence_get(subscribers, index + 1)?
                    .same_bits(selector)
            {
                return self.reactive_remove_at(subscribers, index);
            } else {
                index += 2;
            }
        }
        Ok(())
    }
    fn reactive_remove_at(&mut self, subscribers: Value, index: u32) -> Result<(), HeapError> {
        let len = self.sequence_len(subscribers)?;
        let a = self.sequence_get(subscribers, len - 2)?;
        let b = self.sequence_get(subscribers, len - 1)?;
        self.set_field(subscribers, index, a)?;
        self.set_field(subscribers, index + 1, b)?;
        self.array_pop(subscribers)?;
        self.array_pop(subscribers)?;
        Ok(())
    }

    fn derived_cycle_input(
        &self,
        output: PropertyId,
        mut source: PropertyId,
        writes: Option<&dyn PropertyWrites>,
    ) -> Result<Option<PropertyId>, HeapError> {
        if source.host.same_bits(output.host)
            && (source.field == output.field || source.field == u32::MAX)
        {
            return Err(HeapError::BindingCycle);
        }
        if source.field != u32::MAX {
            source = if let Some(writes) = writes {
                PropertyRead::proposed(self, writes)
                    .source_avoiding(source, Some(output))?
                    .0
            } else {
                self.property_source(source)?.0
            };
        }
        if source.host.same_bits(output.host)
            && (source.field == output.field || source.field == u32::MAX)
        {
            return Err(HeapError::BindingCycle);
        }
        Ok(self
            .object(self.key(source.host)?)?
            .derived
            .then_some(source))
    }

    pub(super) fn check_derived_cycle(
        &mut self,
        output: PropertyId,
        source: PropertyId,
    ) -> Result<(), HeapError> {
        self.check_derived_configuration_cycle(output, source, None)
    }
    pub(super) fn check_derived_configuration_cycle(
        &mut self,
        output: PropertyId,
        source: PropertyId,
        writes: Option<&dyn PropertyWrites>,
    ) -> Result<(), HeapError> {
        self.property_work.clear();
        if let Some(source) = self.derived_cycle_input(output, source, writes)? {
            self.work_push(source)?;
        }
        let mut cursor = 0;
        while cursor < self.property_work.len() {
            let current = self.property_work[cursor];
            cursor += 1;
            let dependencies = self.host_payload(current.host, DerivedSlot::Dependencies as u32)?;
            if dependencies.same_bits(Value::NULL) {
                continue;
            }
            let mut inputs = DependencyCursor::new(dependencies);
            while let Some((host, selector)) = inputs.next(self)? {
                let next = PropertyId {
                    host,
                    field: selector.as_int().unwrap() as u32,
                };
                // Ordinary model endpoints can close a binding cycle, but have
                // no derived inputs. Do not add them to the visited work list.
                let Some(next) = self.derived_cycle_input(output, next, writes)? else {
                    continue;
                };
                if !self
                    .property_work
                    .iter()
                    .any(|p| p.host.same_bits(next.host) && p.field == next.field)
                {
                    self.work_push(next)?;
                }
            }
        }
        Ok(())
    }

    pub fn commit_derived(&mut self, output: Value, result: Value) -> Result<(), HeapError> {
        let dependencies = self.host_payload(output, DerivedSlot::Candidate as u32)?;
        let mut inputs = DependencyCursor::new(dependencies);
        while let Some((host, selector)) = inputs.next(self)? {
            self.check_derived_cycle(
                PropertyId {
                    host: output,
                    field: 0,
                },
                PropertyId {
                    host,
                    field: selector.as_int().unwrap() as u32,
                },
            )?;
        }
        let old = self.host_payload(output, DerivedSlot::Dependencies as u32)?;
        if !self.same_derived_dependencies(old, dependencies)? {
            let weak = self.host_payload(output, DerivedSlot::Weak as u32)?;
            // Admit new edges before publishing the computed value. A failed evaluation
            // never changes the last valid output; retained metadata stays GC-owned.
            let mut inputs = DependencyCursor::new(dependencies);
            while let Some((source, selector)) = inputs.next(self)? {
                let key = self.key(source)?;
                let mut subscribers = self.object(key)?.reactive_subscribers;
                if subscribers.same_bits(Value::NULL) {
                    subscribers = self.alloc_array(&[])?;
                    self.barrier(subscribers)?;
                    self.objects.get_mut(key).unwrap().reactive_subscribers = subscribers;
                }
                self.packed_pair_push(subscribers, weak, selector)?;
                self.reconnect_derived(source)?;
            }
            if !old.same_bits(Value::NULL) {
                let mut inputs = DependencyCursor::new(old);
                while let Some((source, selector)) = inputs.next(self)? {
                    // Remove one old occurrence, retaining the new subscription.
                    self.reactive_remove(source, weak, selector)?;
                }
            }
            self.set_host_payload(output, DerivedSlot::Dependencies as u32, dependencies)?;
            self.set_host_payload(output, DerivedSlot::Candidate as u32, old)?;
        }
        let property = PropertyId {
            host: output,
            field: 0,
        };
        if !self.host_property(property)?.same_bits(result) {
            self.publish_host_property(property, result)?;
        }
        self.end_derived(output)
    }

    fn same_derived_dependencies(&self, old: Value, candidate: Value) -> Result<bool, HeapError> {
        if old.same_bits(Value::NULL) {
            return Ok(false);
        }
        let Payload::Dependencies(old) = &self.object(self.key(old)?)?.payload else {
            unreachable!()
        };
        let Payload::Dependencies(candidate) = &self.object(self.key(candidate)?)?.payload else {
            unreachable!()
        };
        if old.values.len != candidate.values.len {
            return Ok(false);
        }
        let mut offset = 0;
        while offset < old.values.len {
            let before = old.values.segment(offset);
            let after = candidate.values.segment(offset);
            let count = before.len().min(after.len());
            self.property_traversal_work
                .set(self.property_traversal_work.get() + count as u64 * 8);
            if !before[..count]
                .iter()
                .zip(&after[..count])
                .all(|(before, after)| before.same_bits(*after))
            {
                return Ok(false);
            }
            offset += count as u32;
        }
        // Stable captures retain the already-admitted reverse edges. Their
        // source identities include full generations and selectors remain exact.
        Ok(true)
    }

    pub(crate) fn invalidate_reactive(
        &mut self,
        value: Value,
        field: Option<u32>,
    ) -> Result<(), HeapError> {
        let key = self.key(value)?;
        let source = self.object(key)?.property_owner.unwrap_or(key);
        let object = self.object(source)?;
        if !object.model && object.observable_fields == 0 {
            return Ok(());
        }
        if let Some(output) = self.reactive_capture
            && let dependencies = self.host_payload(output, DerivedSlot::Candidate as u32)?
            && !dependencies.same_bits(Value::NULL)
        {
            let source_value = Value::object(self.id, source.data().as_ffi());
            let Payload::Dependencies(capture) = &self.object(self.key(dependencies)?)?.payload
            else {
                unreachable!()
            };
            if capture.overlaps(source_value, field) {
                self.enqueue_derived(self.key(output)?);
            }
        }
        let subscribers = self.object(source)?.reactive_subscribers;
        if subscribers.same_bits(Value::NULL) {
            return Ok(());
        }
        let mut i = 0;
        while i < self.sequence_len(subscribers)? {
            self.property_traversal_work
                .set(self.property_traversal_work.get() + 8);
            let weak = self.sequence_get(subscribers, i)?;
            let selector = self.sequence_get(subscribers, i + 1)?;
            let Some(output) = self.subscriber_target(weak)? else {
                self.reactive_remove_at(subscribers, i)?;
                continue;
            };
            i += 2;
            if selector.as_int() != Some(-1)
                && field.is_some_and(|f| selector.as_int() != Some(i64::from(f)))
            {
                continue;
            }
            let key = self.key(output)?;
            self.enqueue_derived(key);
        }
        Ok(())
    }

    pub fn disconnect_derived(&mut self, output: Value) -> Result<(), HeapError> {
        let dependencies = self.host_payload(output, DerivedSlot::Dependencies as u32)?;
        let weak = self.host_payload(output, DerivedSlot::Weak as u32)?;
        if !dependencies.same_bits(Value::NULL) {
            let mut inputs = DependencyCursor::new(dependencies);
            while let Some((source, selector)) = inputs.next(self)? {
                self.reactive_remove(source, weak, selector)?;
            }
        }
        self.set_host_payload(output, DerivedSlot::Dependencies as u32, Value::NULL)?;
        self.set_host_payload(output, DerivedSlot::Suspended as u32, Value::NULL)?;
        let key = self.key(output)?;
        self.objects.get_mut(key).unwrap().derived_dirty = false;
        Ok(())
    }

    /// A retired host can no longer be read. Freeze computations that subscribed
    /// to it, including ones already queued by its last interaction update.
    pub(super) fn disconnect_reactive_source(&mut self, source: Value) -> Result<(), HeapError> {
        let subscribers = self.object(self.key(source)?)?.reactive_subscribers;
        if subscribers.same_bits(Value::NULL) {
            return Ok(());
        }
        while self.sequence_len(subscribers)? != 0 {
            let weak = self.sequence_get(subscribers, 0)?;
            let selector = self.sequence_get(subscribers, 1)?;
            if let Some(output) = self.subscriber_target(weak)? {
                self.disconnect_derived(output)?;
            } else {
                self.reactive_remove(source, weak, selector)?;
            }
        }
        Ok(())
    }

    pub(super) fn reconnect_derived(&mut self, output: Value) -> Result<(), HeapError> {
        let key = self.key(output)?;
        if self.object(key)?.derived
            && self
                .host_payload(output, DerivedSlot::Suspended as u32)?
                .as_bool()
                == Some(true)
        {
            self.set_host_payload(output, DerivedSlot::Suspended as u32, Value::NULL)?;
            self.enqueue_derived(key);
        }
        Ok(())
    }

    /// Detaching the last consumer releases computed inputs transitively. Keep
    /// shared computations live; reconnecting a retained reference refreshes it.
    pub(super) fn disconnect_unobserved_derived(&mut self, output: Value) -> Result<(), HeapError> {
        if !self.object(self.key(output)?)?.derived {
            return Ok(());
        }
        self.derived_disconnect_work.clear();
        self.reserve_derived_disconnect(1)?;
        self.derived_disconnect_work.push(output);
        while let Some(output) = self.derived_disconnect_work.pop() {
            self.property_traversal_work
                .set(self.property_traversal_work.get() + 16);
            let object = self.object(self.key(output)?)?;
            if !object.derived {
                continue;
            }
            let downstream = object.reactive_subscribers;
            let bound = self.subscribers(PropertyId {
                host: output,
                field: 0,
            })?;
            if (!downstream.same_bits(Value::NULL) && self.sequence_len(downstream)? != 0)
                || (!bound.same_bits(Value::NULL) && self.sequence_len(bound)? != 0)
            {
                continue;
            }
            let dependencies = self.host_payload(output, DerivedSlot::Dependencies as u32)?;
            if dependencies.same_bits(Value::NULL) {
                continue;
            }
            let len = self.sequence_len(dependencies)?;
            let needed = self.derived_disconnect_work.len() + len as usize / 2;
            self.reserve_derived_disconnect(needed)?;
            for index in (0..len).step_by(2) {
                self.property_traversal_work
                    .set(self.property_traversal_work.get() + 4);
                self.derived_disconnect_work
                    .push(self.sequence_get(dependencies, index)?);
            }
            self.disconnect_derived(output)?;
            self.set_host_payload(output, DerivedSlot::Suspended as u32, Value::bool(true))?;
        }
        Ok(())
    }

    fn reserve_derived_disconnect(&mut self, needed: usize) -> Result<(), HeapError> {
        let old = self.derived_disconnect_work.capacity();
        if needed <= old {
            return Ok(());
        }
        let next = needed.max(old * 2).max(8);
        self.ensure_bytes(((next - old) * std::mem::size_of::<Value>()) as u64)?;
        self.derived_disconnect_work
            .try_reserve_exact(next - self.derived_disconnect_work.len())
            .map_err(|_| HeapError::AllocationFailure)?;
        let bytes =
            ((self.derived_disconnect_work.capacity() - old) * std::mem::size_of::<Value>()) as u64;
        self.usage.charge(bytes);
        self.reserved += bytes;
        Ok(())
    }

    /// Reserve an upper bound before publishing any replacement connection.
    /// Every dependency edge can enter the later stack once before its owner's
    /// dependency list is disconnected; repeated inputs need no second walk.
    pub(super) fn prepare_derived_disconnect(&mut self, output: Value) -> Result<(), HeapError> {
        if !self.object(self.key(output)?)?.derived {
            return Ok(());
        }
        self.property_work.clear();
        self.work_push(PropertyId {
            host: output,
            field: 0,
        })?;
        let mut cursor = 0;
        let mut edges = 1usize;
        while cursor < self.property_work.len() {
            let output = self.property_work[cursor].host;
            cursor += 1;
            let dependencies = self.host_payload(output, DerivedSlot::Dependencies as u32)?;
            if dependencies.same_bits(Value::NULL) {
                continue;
            }
            let len = self.sequence_len(dependencies)?;
            edges = edges
                .checked_add(len as usize / 2)
                .ok_or(HeapError::OutOfBounds)?;
            for index in (0..len).step_by(2) {
                let source = self.sequence_get(dependencies, index)?;
                if self.object(self.key(source)?)?.derived
                    && !self
                        .property_work
                        .iter()
                        .any(|item| item.host.same_bits(source))
                {
                    self.work_push(PropertyId {
                        host: source,
                        field: 0,
                    })?;
                }
            }
        }
        self.reserve_derived_disconnect(edges)?;
        self.property_work.clear();
        Ok(())
    }

    /// Script-visible service collections are observed but owned by their host.
    pub fn observe_readonly_collection(&mut self, value: Value) -> Result<(), HeapError> {
        let key = self.key(value)?;
        let object = self.objects.get_mut(key).unwrap();
        object.model = true;
        object.readonly_collection = true;
        self.mutation_revision = self.mutation_revision.wrapping_add(1);
        Ok(())
    }

    pub fn collection_is_readonly(&self, value: Value) -> Result<bool, HeapError> {
        Ok(self.object(self.key(value)?)?.readonly_collection)
    }

    pub fn reactive_evaluating(&self) -> bool {
        self.reactive_capture.is_some()
    }

    pub fn derived_pending(&self) -> bool {
        !self.reactive_queue.is_empty() || self.reactive_capture.is_some()
    }

    /// Queue a host-owned observer at an explicit cooperative boundary.
    pub fn schedule_derived(&mut self, output: Value) -> Result<(), HeapError> {
        let key = self.key(output)?;
        if self.object(key)?.derived_dirty {
            self.reactive_queue.retain(|queued| *queued != key);
            self.objects.get_mut(key).unwrap().derived_dirty = false;
        }
        self.enqueue_derived(key);
        Ok(())
    }

    pub fn derived_is_queued(&self, output: Value) -> Result<bool, HeapError> {
        Ok(self.object(self.key(output)?)?.derived_dirty)
    }

    /// Read existing queued identities without copying or rooting another graph.
    pub fn pending_derived_outputs(&self) -> impl Iterator<Item = Value> + '_ {
        self.reactive_queue.iter().filter_map(|key| {
            let object = self.objects.get(*key)?;
            (object.derived_dirty
                && !object.dying
                && !(self.gc.cutoff() && object.mark != self.gc.cycle))
                .then(|| Value::object(self.id, key.data().as_ffi()))
        })
    }

    pub fn next_derived(&mut self) -> Result<Option<Value>, HeapError> {
        while let Some(key) = self.reactive_queue.pop_front() {
            let Some(object) = self.objects.get_mut(key) else {
                continue;
            };
            if !object.derived_dirty
                || object.dying
                || (self.gc.cutoff() && object.mark != self.gc.cycle)
            {
                continue;
            }
            object.derived_dirty = false;
            let value = Value::object(self.id, key.data().as_ffi());
            self.root_insertion_barrier(value)?;
            return Ok(Some(value));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_host_payload_keeps_gc_edges_without_invalidating_properties() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let fields = heap.alloc_table().unwrap();
        let name = heap.alloc_string("elapsed").unwrap();
        heap.table_store(fields, 0, name, Value::float(0.)).unwrap();
        let host = heap.alloc_host(fields, &[Value::NULL], None).unwrap();
        heap.observe_host_properties(host, 0, 1).unwrap();
        heap.add_root(RootOwner(1), RootKind::Host, host).unwrap();
        let output = heap.alloc_derived(Value::NULL).unwrap();
        heap.add_root(RootOwner(1), RootKind::Host, output).unwrap();
        heap.begin_derived(output).unwrap();
        heap.observe_read(host, Some(0)).unwrap();
        heap.commit_derived(output, Value::float(0.)).unwrap();
        let child = heap.alloc_array(&[Value::int(42)]).unwrap();
        let key = heap.key(host).unwrap();
        heap.begin_collection().unwrap();
        while heap.object(key).unwrap().mark != heap.gc.cycle || heap.object(key).unwrap().queued {
            heap.gc_step(crate::GcBudget {
                units: 1,
                ..Default::default()
            })
            .unwrap();
        }
        assert_eq!(heap.phase(), GcPhase::Mark);
        let version = heap.info(host).unwrap().content_version;
        heap.set_host_payload(host, 0, child).unwrap();
        assert!(heap.info(host).unwrap().content_version > version);
        assert!(!heap.derived_pending());
        assert!(heap.derived_is_current(output, None).unwrap());
        while heap.phase() != GcPhase::Idle {
            heap.gc_step(crate::GcBudget {
                units: 1,
                ..Default::default()
            })
            .unwrap();
        }
        assert_eq!(heap.sequence_get(child, 0).unwrap().as_int(), Some(42));
        heap.publish_host_property(PropertyId { host, field: 0 }, Value::float(1.))
            .unwrap();
        assert!(heap.next_derived().unwrap().unwrap().same_bits(output));
        heap.set_host_payload(host, 0, Value::NULL).unwrap();
        collect(&mut heap);
        assert!(heap.info(child).is_err());
    }

    #[test]
    fn derived_freshness_follows_only_its_inputs_without_consuming_dirty_work() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let source = heap.alloc_array(&[]).unwrap();
        heap.observe_readonly_collection(source).unwrap();
        let upstream = heap.alloc_derived(Value::NULL).unwrap();
        heap.begin_derived(upstream).unwrap();
        heap.observe_read(source, None).unwrap();
        heap.commit_derived(upstream, Value::int(1)).unwrap();
        let output = heap.alloc_derived(Value::NULL).unwrap();
        heap.begin_derived(output).unwrap();
        heap.observe_read(upstream, Some(0)).unwrap();
        heap.commit_derived(output, Value::int(1)).unwrap();
        let unrelated = heap.alloc_derived(Value::NULL).unwrap();
        heap.begin_derived(unrelated).unwrap();
        heap.commit_derived(unrelated, Value::NULL).unwrap();
        assert!(heap.derived_is_current(output, None).unwrap());
        heap.invalidate_reactive(source, None).unwrap();
        assert!(!heap.derived_is_current(output, None).unwrap());
        assert!(heap.derived_is_current(unrelated, None).unwrap());
        assert!(heap.next_derived().unwrap().unwrap().same_bits(upstream));
        assert!(!heap.derived_is_current(output, Some(upstream)).unwrap());
        heap.begin_derived(upstream).unwrap();
        assert!(!heap.derived_is_current(output, None).unwrap());
        heap.observe_read(source, None).unwrap();
        heap.commit_derived(upstream, Value::int(1)).unwrap();
        assert!(heap.derived_is_current(output, None).unwrap());
        assert!(heap.next_derived().unwrap().is_none());
        heap.publish_host_property(
            PropertyId {
                host: upstream,
                field: 0,
            },
            Value::int(2),
        )
        .unwrap();
        assert!(!heap.derived_is_current(output, None).unwrap());
        assert!(heap.next_derived().unwrap().unwrap().same_bits(output));
        heap.begin_derived(output).unwrap();
        heap.observe_read(upstream, Some(0)).unwrap();
        heap.commit_derived(output, Value::int(2)).unwrap();
        assert!(heap.derived_is_current(output, None).unwrap());
        assert!(heap.next_derived().unwrap().is_none());
        heap.begin_untracked(output).unwrap();
        assert!(!heap.derived_is_current(output, None).unwrap());
        heap.end_derived(output).unwrap();
        assert!(heap.derived_is_current(output, None).unwrap());
    }

    fn model_fields(heap: &mut Heap, n: u32) -> Value {
        let source = heap.alloc_table().unwrap();
        for field in 0..n {
            heap.table_store(source, field, Value::int(field.into()), Value::int(0))
                .unwrap();
        }
        heap.observe_model_object(source).unwrap();
        source
    }
    fn capture_len(heap: &Heap, output: Value) -> u32 {
        let candidate = heap
            .host_payload(output, DerivedSlot::Candidate as u32)
            .unwrap();
        heap.sequence_len(candidate).unwrap()
    }

    #[test]
    fn dependency_index_preserves_nested_capture_and_field_conflicts() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let source = model_fields(&mut heap, 3);
        let outer = heap.alloc_derived(Value::NULL).unwrap();
        let inner = heap.alloc_derived(Value::NULL).unwrap();
        heap.begin_derived(outer).unwrap();
        heap.observe_read(source, Some(0)).unwrap();
        heap.begin_derived(inner).unwrap();
        heap.observe_read(source, Some(1)).unwrap();
        heap.observe_read(source, Some(1)).unwrap();
        assert_eq!(capture_len(&heap, inner), 2);
        assert!(heap.observe_write(source, Some(0)).is_ok());
        assert_eq!(
            heap.observe_write(source, Some(1)),
            Err(HeapError::BindingCycle)
        );
        heap.commit_derived(inner, Value::int(1)).unwrap();
        heap.observe_read(source, Some(0)).unwrap();
        assert_eq!(capture_len(&heap, outer), 2);
        heap.observe_read(source, Some(1)).unwrap();
        assert_eq!(capture_len(&heap, outer), 4);
        assert!(heap.observe_write(source, Some(2)).is_ok());
        assert_eq!(
            heap.observe_write(source, None),
            Err(HeapError::BindingCycle)
        );
        heap.observe_read(source, None).unwrap();
        assert_eq!(
            heap.observe_write(source, Some(2)),
            Err(HeapError::BindingCycle)
        );
        heap.begin_untracked(inner).unwrap();
        heap.observe_read(source, Some(2)).unwrap();
        heap.end_derived(inner).unwrap();
        assert_eq!(capture_len(&heap, outer), 6);
        heap.commit_derived(outer, Value::int(2)).unwrap();
        heap.begin_derived(outer).unwrap();
        heap.observe_read(source, Some(2)).unwrap();
        heap.end_derived(outer).unwrap(); // A failed/abandoned candidate never replaces dependencies.
        let dependencies = heap
            .host_payload(outer, DerivedSlot::Dependencies as u32)
            .unwrap();
        assert_eq!(heap.sequence_len(dependencies).unwrap(), 6);
        assert_eq!(
            heap.host_property(PropertyId {
                host: outer,
                field: 0
            })
            .unwrap()
            .as_int(),
            Some(2)
        );
    }

    #[test]
    fn dependency_block_tail_keeps_alias_checks_and_replacement_subscriptions() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let sources: Vec<_> = (0..67).map(|_| model_fields(&mut heap, 2)).collect();
        let output = heap.alloc_derived(Value::NULL).unwrap();
        heap.begin_derived(output).unwrap();
        for source in &sources {
            heap.observe_read(*source, Some(0)).unwrap();
        }
        heap.commit_derived(output, Value::int(0)).unwrap();
        assert!(heap.derived_is_current(output, None).unwrap());

        // The last pair is in a partially filled third block. An equal-value
        // alias must still expose a dirty ancestor, without direct invalidation.
        let tail = sources[66];
        let upstream = heap.alloc_derived(Value::NULL).unwrap();
        heap.begin_derived(upstream).unwrap();
        heap.observe_read(tail, Some(1)).unwrap();
        heap.commit_derived(upstream, Value::int(0)).unwrap();
        let reference = heap
            .alloc_property_ref(
                PropertyId {
                    host: upstream,
                    field: 0,
                },
                false,
            )
            .unwrap();
        heap.bind_host_property(
            PropertyId {
                host: tail,
                field: 0,
            },
            reference,
        )
        .unwrap();
        heap.invalidate_reactive(tail, Some(1)).unwrap();
        assert!(
            !heap
                .object(heap.key(output).unwrap())
                .unwrap()
                .derived_dirty
        );
        assert!(!heap.derived_is_current(output, None).unwrap());

        // Replacing a cross-block set removes every old subscription, including
        // the partial tail, while abandoning a later capture keeps the new set.
        heap.begin_derived(output).unwrap();
        heap.observe_read(sources[0], Some(0)).unwrap();
        heap.commit_derived(output, Value::int(1)).unwrap();
        assert!(heap.derived_is_current(output, None).unwrap());
        for source in &sources[1..] {
            heap.invalidate_reactive(*source, Some(0)).unwrap();
        }
        assert!(heap.derived_is_current(output, None).unwrap());
        heap.begin_derived(output).unwrap();
        for source in &sources[1..] {
            heap.observe_read(*source, Some(0)).unwrap();
        }
        heap.end_derived(output).unwrap();
        assert!(heap.derived_is_current(output, None).unwrap());
        heap.invalidate_reactive(sources[0], Some(0)).unwrap();
        assert!(!heap.derived_is_current(output, None).unwrap());
        heap.disconnect_derived(output).unwrap();
        assert!(heap.derived_is_current(output, None).unwrap());
        heap.invalidate_reactive(sources[0], Some(0)).unwrap();
        assert!(heap.derived_is_current(output, None).unwrap());
    }

    #[test]
    fn dependency_admission_is_atomic_and_gc_releases_index_incrementally() {
        let mut heap = Heap::new(
            SessionEpoch(1),
            HeapLimits {
                max_bytes: 1_000_000,
                ..Default::default()
            },
        )
        .unwrap();
        let source = model_fields(&mut heap, 100);
        let output = heap.alloc_derived(Value::NULL).unwrap();
        let root = heap.add_root(RootOwner(1), RootKind::Host, output).unwrap();
        heap.begin_derived(output).unwrap();
        for field in 0..4 {
            heap.observe_read(source, Some(field)).unwrap();
        }
        let bytes = heap.stats().charged_bytes;
        let reserved = heap
            .budget_usage()
            .reserve(
                heap.budget_usage().remaining_bytes()
                    - Values::growth_bytes(8, 2)
                    - crate::dependencies::INDEX_ENTRY_BYTES
                    + 1,
            )
            .unwrap();
        assert!(matches!(
            heap.observe_read(source, Some(4)),
            Err(HeapError::ResourceLimit { .. })
        ));
        assert_eq!(capture_len(&heap, output), 8);
        heap.observe_read(source, Some(0)).unwrap(); // Duplicate reads need no capacity.
        drop(reserved);
        assert_eq!(heap.stats().charged_bytes, bytes);
        for field in 4..100 {
            heap.observe_read(source, Some(field)).unwrap();
        }
        let candidate = heap
            .host_payload(output, DerivedSlot::Candidate as u32)
            .unwrap();
        assert_eq!(
            heap.array_push(candidate, Value::NULL),
            Err(HeapError::WrongKind)
        );
        assert_eq!(
            heap.set_field(candidate, 0, Value::NULL),
            Err(HeapError::WrongKind)
        );
        collect(&mut heap); // Only the candidate's strong values retain the source.
        assert!(heap.info(source).is_ok());
        heap.end_derived(output).unwrap();
        heap.release_root(root).unwrap();
        heap.begin_collection().unwrap();
        let mut examined_release = false;
        let mut previous_index_len = 100;
        while heap.phase() != GcPhase::Idle {
            let progress = heap
                .gc_step(crate::GcBudget {
                    units: 1,
                    ..Default::default()
                })
                .unwrap();
            assert!(progress.units <= 1);
            if let Ok(key) = heap.key(candidate)
                && let Some(object) = heap.objects.get(key)
                && let Payload::Dependencies(d) = &object.payload
            {
                assert!(previous_index_len - d.index.len() <= 1);
                previous_index_len = d.index.len();
                examined_release |= d.index.len() < 100;
            }
        }
        assert!(examined_release);
        assert_eq!(heap.stats().payload_bytes, 0);
        assert_eq!(heap.stats().live_objects, 0);
        assert!(heap.info(source).is_err());
        let replacement = model_fields(&mut heap, 1);
        let output = heap.alloc_derived(Value::NULL).unwrap();
        heap.begin_derived(output).unwrap();
        heap.observe_read(replacement, Some(0)).unwrap();
        assert_eq!(capture_len(&heap, output), 2);
    }

    #[test]
    fn derived_cycle_check_still_rejects_normal_fields_and_bound_aliases() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let source = model_fields(&mut heap, 2);
        let alias = model_fields(&mut heap, 1);
        let source_ref = heap
            .alloc_property_ref(
                PropertyId {
                    host: source,
                    field: 0,
                },
                true,
            )
            .unwrap();
        heap.bind_host_property(
            PropertyId {
                host: alias,
                field: 0,
            },
            source_ref,
        )
        .unwrap();
        let output = heap.alloc_derived(Value::NULL).unwrap();
        heap.begin_derived(output).unwrap();
        heap.observe_read(alias, Some(0)).unwrap();
        heap.commit_derived(output, Value::int(42)).unwrap();
        let reference = heap
            .alloc_property_ref(
                PropertyId {
                    host: output,
                    field: 0,
                },
                false,
            )
            .unwrap();
        assert_eq!(
            heap.bind_host_property(
                PropertyId {
                    host: alias,
                    field: 0
                },
                reference
            ),
            Err(HeapError::BindingCycle)
        );
        assert_eq!(
            heap.bind_host_property(
                PropertyId {
                    host: source,
                    field: 0
                },
                reference
            ),
            Err(HeapError::BindingCycle)
        );
        heap.bind_host_property(
            PropertyId {
                host: source,
                field: 1,
            },
            reference,
        )
        .unwrap();
        heap.begin_derived(output).unwrap();
        heap.observe_read(source, Some(1)).unwrap();
        assert_eq!(
            heap.commit_derived(output, Value::NULL),
            Err(HeapError::BindingCycle)
        );
        heap.end_derived(output).unwrap();
        assert_eq!(
            heap.host_property(PropertyId {
                host: output,
                field: 0
            })
            .unwrap()
            .as_int(),
            Some(42)
        );
    }

    #[test]
    fn dependency_snapshot_rebuilds_remapped_indexes_and_nested_captures() {
        use crate::heap::snapshot as graph;
        use crate::snapshot::{SnapshotMap, SnapshotRegistry, VmSnapshot};
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let slots = crate::FrameSlots::new(
            &mut heap,
            crate::FrameSlotsLimits {
                initial_capacity: 8,
                max_capacity: 128,
                max_window_len: 64,
            },
        )
        .unwrap();
        let source = model_fields(&mut heap, 97);
        let idle = heap.alloc_derived(Value::NULL).unwrap();
        heap.add_root(RootOwner(1), RootKind::Host, idle).unwrap();
        for _ in 0..2 {
            heap.begin_derived(idle).unwrap();
            heap.observe_read(source, Some(1)).unwrap();
            heap.commit_derived(idle, Value::int(1)).unwrap();
        }
        let outer = heap.alloc_derived(Value::NULL).unwrap();
        let inner = heap.alloc_derived(Value::NULL).unwrap();
        heap.begin_derived(outer).unwrap();
        for field in 0..80 {
            heap.observe_read(source, Some(field)).unwrap();
        }
        heap.begin_derived(inner).unwrap();
        heap.observe_read(source, Some(1)).unwrap();
        let (active, evaluation) = heap.snapshot_reactive_state();
        let registry = SnapshotRegistry::default();
        let mut image = VmSnapshot {
            version: VmSnapshot::FORMAT_VERSION,
            code: [0; 32],
            source_heap: heap.id().get(),
            source_epoch: 1,
            records: Vec::new(),
            bytes: 0,
        };
        let mut capture = graph::GraphCapture::new(&heap).unwrap();
        while !capture.step(&heap, &slots, &registry, &mut image).unwrap() {}
        let image = VmSnapshot::decode(&image.encode().unwrap()).unwrap();
        let mut restored = Heap::new(SessionEpoch(2), HeapLimits::default()).unwrap();
        // Force a different slot mapping rather than accidentally testing equal raw keys.
        for _ in 0..7 {
            restored.alloc_array(&[]).unwrap();
        }
        let mut restored_slots = crate::FrameSlots::new(
            &mut restored,
            crate::FrameSlotsLimits {
                initial_capacity: 8,
                max_capacity: 128,
                max_window_len: 64,
            },
        )
        .unwrap();
        let mut map = SnapshotMap::new(&restored, &image);
        for record in &image.records {
            graph::allocate(
                &mut restored,
                &mut restored_slots,
                &mut map,
                record,
                &registry,
            )
            .unwrap();
        }
        for record in &image.records {
            graph::connect(&mut restored, &mut restored_slots, &mut map, record).unwrap();
        }
        for record in &image.records {
            graph::validate(&restored, &map, record).unwrap();
        }
        let source2 = map.value(source).unwrap();
        let idle = map.value(idle).unwrap();
        let scratch = restored
            .host_payload(idle, DerivedSlot::Candidate as u32)
            .unwrap();
        assert_eq!(restored.sequence_len(scratch).unwrap(), 0);
        assert_ne!(
            source.object_parts().unwrap().1,
            source2.object_parts().unwrap().1
        );
        let outer = map.value(outer).unwrap();
        let inner = map.value(inner).unwrap();
        restored
            .restore_reactive_state(active.map(|v| map.value(v).unwrap()), evaluation)
            .unwrap();
        restored.observe_read(source2, Some(1)).unwrap();
        assert_eq!(capture_len(&restored, inner), 2);
        assert_eq!(
            restored.observe_write(source2, Some(1)),
            Err(HeapError::BindingCycle)
        );
        restored.commit_derived(inner, Value::int(1)).unwrap();
        for field in 0..80 {
            restored.observe_read(source2, Some(field)).unwrap();
        }
        assert_eq!(capture_len(&restored, outer), 160);
        restored.observe_read(source2, Some(80)).unwrap();
        assert_eq!(capture_len(&restored, outer), 162);
        restored.commit_derived(outer, Value::int(2)).unwrap();
        assert!(!restored.reactive_evaluating());
        restored.invalidate_reactive(source2, Some(0)).unwrap();
        assert!(restored.next_derived().unwrap().unwrap().same_bits(outer));
        assert!(restored.next_derived().unwrap().is_none());
        let before = restored.stats().live_objects;
        restored.begin_derived(idle).unwrap();
        restored.observe_read(source2, Some(1)).unwrap();
        restored.commit_derived(idle, Value::int(1)).unwrap();
        assert_eq!(restored.stats().live_objects, before);
    }

    #[test]
    #[ignore = "release measurement of dependency capture"]
    fn model_dependency_capture_cost() {
        let sizes = std::env::var("REEL_COST_ROWS").unwrap_or_else(|_| "1000,10000,30000".into());
        for n in sizes.split(',').map(|s| s.parse::<usize>().unwrap()) {
            let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
            let roots = heap.alloc_array(&[]).unwrap();
            heap.add_root(RootOwner(1), RootKind::Host, roots).unwrap();
            let mut sources = Vec::with_capacity(n);
            let key = heap.alloc_string("id").unwrap();
            let start = std::time::Instant::now();
            for i in 0..n {
                let source = heap.alloc_table().unwrap();
                heap.table_store(source, 0, key, Value::int(i as i64))
                    .unwrap();
                heap.observe_model_object(source).unwrap();
                heap.array_push(roots, source).unwrap();
                sources.push(source);
            }
            let generation = start.elapsed();
            let output = heap.alloc_derived(Value::NULL).unwrap();
            heap.add_root(RootOwner(1), RootKind::Host, output).unwrap();
            let before = heap.stats().charged_bytes;
            heap.begin_derived(output).unwrap();
            let start = std::time::Instant::now();
            let allocation = allocation_counter::measure(|| {
                for source in &sources {
                    heap.observe_read(*source, Some(0)).unwrap();
                }
            });
            let capture = start.elapsed();
            let candidate = heap
                .host_payload(output, DerivedSlot::Candidate as u32)
                .unwrap();
            assert_eq!(heap.sequence_len(candidate).unwrap() as usize, n * 2);
            // Separate repeated sequence lookup and endpoint normalization from
            // subscription storage. These diagnostic passes do not publish a
            // value, run scripts, allocate subscriptions or change the capture.
            let start = std::time::Instant::now();
            for i in (0..heap.sequence_len(candidate).unwrap()).step_by(2) {
                std::hint::black_box((
                    heap.sequence_get(candidate, i).unwrap(),
                    heap.sequence_get(candidate, i + 1).unwrap(),
                ));
            }
            let dependency_reads = start.elapsed();
            let start = std::time::Instant::now();
            for source in &sources {
                std::hint::black_box(
                    heap.property_source(PropertyId {
                        host: *source,
                        field: 0,
                    })
                    .unwrap(),
                );
            }
            let endpoint_resolution = start.elapsed();
            let start = std::time::Instant::now();
            for i in (0..heap.sequence_len(candidate).unwrap()).step_by(2) {
                heap.check_derived_cycle(
                    PropertyId {
                        host: output,
                        field: 0,
                    },
                    PropertyId {
                        host: heap.sequence_get(candidate, i).unwrap(),
                        field: heap
                            .sequence_get(candidate, i + 1)
                            .unwrap()
                            .as_int()
                            .unwrap() as u32,
                    },
                )
                .unwrap();
            }
            let cycle_validation = start.elapsed();
            let objects_before_commit = heap.stats().live_objects;
            let start = std::time::Instant::now();
            let commit_allocation = allocation_counter::measure(|| {
                heap.commit_derived(output, Value::NULL).unwrap();
            });
            let commit = start.elapsed();
            let commit_objects = heap.stats().live_objects - objects_before_commit;
            eprintln!(
                "model-dependency-phases rows={n} dependency_reads={dependency_reads:?} endpoint_resolution={endpoint_resolution:?} cycle_validation={cycle_validation:?} commit_alloc_live={} commit_alloc_peak={} commit_objects={commit_objects}",
                commit_allocation.bytes_current, commit_allocation.bytes_max,
            );
            let before_freshness = heap.stats().charged_bytes;
            let start = std::time::Instant::now();
            assert!(heap.derived_is_current(output, None).unwrap());
            let freshness = start.elapsed();
            let freshness_charge = heap.stats().charged_bytes - before_freshness;
            let start = std::time::Instant::now();
            for _ in 0..100 {
                assert!(heap.derived_is_current(output, None).unwrap());
            }
            let freshness_stable = start.elapsed() / 100;
            let downstream = heap.alloc_derived(Value::NULL).unwrap();
            heap.begin_derived(downstream).unwrap();
            heap.observe_read(output, Some(0)).unwrap();
            let start = std::time::Instant::now();
            heap.commit_derived(downstream, Value::NULL).unwrap();
            let downstream_commit = start.elapsed();
            let stats = heap.stats();
            eprintln!(
                "model-dependencies rows={n} generation={generation:?} capture={capture:?} commit={commit:?} downstream_commit={downstream_commit:?} freshness_cold={freshness:?} freshness_stable={freshness_stable:?} freshness_charge={freshness_charge} capture_alloc_live={} capture_alloc_peak={} added_charge={} charge={} peak_charge={}",
                allocation.bytes_current,
                allocation.bytes_max,
                stats.charged_bytes - before,
                stats.charged_bytes,
                stats.peak_charged_bytes
            );
        }
    }

    fn collect(heap: &mut Heap) {
        heap.begin_collection().unwrap();
        while heap.phase() != GcPhase::Idle {
            heap.gc_step(crate::GcBudget {
                units: 1,
                ..Default::default()
            })
            .unwrap();
        }
    }

    #[test]
    fn derived_subscriptions_coalesce_without_allocation_and_remain_weak() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let fields = heap.alloc_table().unwrap();
        let key = heap.alloc_string("value").unwrap();
        heap.table_store(fields, 0, key, Value::int(0)).unwrap();
        let source = heap.alloc_host(fields, &[], None).unwrap();
        heap.observe_host_properties(source, 1, 0).unwrap();
        let source_root = heap.add_root(RootOwner(1), RootKind::Host, source).unwrap();
        let output = heap.alloc_derived(Value::NULL).unwrap();
        let output_root = heap.add_root(RootOwner(1), RootKind::Host, output).unwrap();
        heap.begin_derived(output).unwrap();
        heap.observe_read(source, Some(0)).unwrap();
        heap.commit_derived(output, Value::int(0)).unwrap();
        let property = PropertyId {
            host: source,
            field: 0,
        };
        heap.write_host_property(property, Value::int(1), true)
            .unwrap();
        assert!(heap.next_derived().unwrap().unwrap().same_bits(output));
        let before = heap.stats();
        let started = std::time::Instant::now();
        for n in 2..=2001 {
            heap.write_host_property(property, Value::int(n), true)
                .unwrap();
        }
        eprintln!(
            "2000 observable writes coalesced into one derivation: {:?}; added heap chunks={}",
            started.elapsed(),
            heap.stats().chunk_allocations - before.chunk_allocations
        );
        assert_eq!(heap.stats().chunk_allocations, before.chunk_allocations);
        assert_eq!(heap.stats().charged_bytes, before.charged_bytes);
        // Pending invalidation is weak; only the result's forward edges keep inputs alive.
        heap.release_root(source_root).unwrap();
        collect(&mut heap);
        assert!(heap.info(source).is_ok());
        let source_root = heap.add_root(RootOwner(1), RootKind::Host, source).unwrap();
        heap.release_root(output_root).unwrap();
        collect(&mut heap);
        assert!(heap.info(output).is_err());
        assert!(heap.next_derived().unwrap().is_none());
        heap.write_host_property(property, Value::int(2002), true)
            .unwrap();
        heap.release_root(source_root).unwrap();
        collect(&mut heap);
        assert_eq!(heap.stats().live_objects, 0);
    }
}
