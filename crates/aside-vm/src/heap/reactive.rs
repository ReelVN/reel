//! Read dependencies share heap ownership and weak reverse subscriptions.
use super::*;

#[derive(Clone, Copy)]
#[repr(u32)]
pub(crate) enum DerivedSlot {
    Closure,
    Dependencies,
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

    pub fn begin_derived(&mut self, output: Value) -> Result<(), HeapError> {
        let candidate = self.alloc_array(&[])?;
        self.begin_capture(output, candidate)
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
        self.set_host_payload(output, DerivedSlot::Candidate as u32, Value::NULL)?;
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
        let selector = field.map_or(-1, i64::from);
        for index in (0..self.sequence_len(dependencies)?).step_by(2) {
            if self.sequence_get(dependencies, index)?.same_bits(source)
                && self.sequence_get(dependencies, index + 1)?.as_int() == Some(selector)
            {
                return Ok(());
            }
        }
        self.packed_pair_push(dependencies, source, Value::int(selector))
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
            .map_or(value, |key| Value::object(self.id, key.data().as_ffi()));
        for i in (0..self.sequence_len(dependencies)?).step_by(2) {
            let selector = self.sequence_get(dependencies, i + 1)?.as_int().unwrap();
            if self.sequence_get(dependencies, i)?.same_bits(source)
                && (selector == -1 || field.is_none_or(|f| selector == i64::from(f)))
            {
                return Err(HeapError::BindingCycle);
            }
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
            if self.sequence_get(subscribers, index)?.same_bits(weak)
                && self
                    .sequence_get(subscribers, index + 1)?
                    .same_bits(selector)
            {
                let len = self.sequence_len(subscribers)?;
                let a = self.sequence_get(subscribers, len - 2)?;
                let b = self.sequence_get(subscribers, len - 1)?;
                self.set_field(subscribers, index, a)?;
                self.set_field(subscribers, index + 1, b)?;
                self.array_pop(subscribers)?;
                self.array_pop(subscribers)?;
                return Ok(());
            } else {
                index += 2;
            }
        }
        Ok(())
    }

    pub(super) fn check_derived_cycle(
        &mut self,
        output: PropertyId,
        source: PropertyId,
    ) -> Result<(), HeapError> {
        self.property_work.clear();
        self.work_push(source)?;
        let mut cursor = 0;
        while cursor < self.property_work.len() {
            let mut current = self.property_work[cursor];
            cursor += 1;
            if current.host.same_bits(output.host)
                && (current.field == output.field || current.field == u32::MAX)
            {
                return Err(HeapError::BindingCycle);
            }
            if current.field != u32::MAX {
                current = self.property_source(current)?.0;
            }
            if current.host.same_bits(output.host) && current.field == output.field {
                return Err(HeapError::BindingCycle);
            }
            if !self.object(self.key(current.host)?)?.derived {
                continue;
            }
            let dependencies = self.host_payload(current.host, DerivedSlot::Dependencies as u32)?;
            if dependencies.same_bits(Value::NULL) {
                continue;
            }
            for i in (0..self.sequence_len(dependencies)?).step_by(2) {
                let next = PropertyId {
                    host: self.sequence_get(dependencies, i)?,
                    field: self.sequence_get(dependencies, i + 1)?.as_int().unwrap() as u32,
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
        for i in (0..self.sequence_len(dependencies)?).step_by(2) {
            self.check_derived_cycle(
                PropertyId {
                    host: output,
                    field: 0,
                },
                PropertyId {
                    host: self.sequence_get(dependencies, i)?,
                    field: self.sequence_get(dependencies, i + 1)?.as_int().unwrap() as u32,
                },
            )?;
        }
        let weak = self.host_payload(output, DerivedSlot::Weak as u32)?;
        // Admit new edges before publishing the computed value. A failed evaluation
        // never changes the last valid output; retained metadata stays GC-owned.
        for i in (0..self.sequence_len(dependencies)?).step_by(2) {
            let source = self.sequence_get(dependencies, i)?;
            let key = self.key(source)?;
            let mut subscribers = self.object(key)?.reactive_subscribers;
            if subscribers.same_bits(Value::NULL) {
                subscribers = self.alloc_array(&[])?;
                self.barrier(subscribers)?;
                self.objects.get_mut(key).unwrap().reactive_subscribers = subscribers;
            }
            self.packed_pair_push(subscribers, weak, self.sequence_get(dependencies, i + 1)?)?;
            self.reconnect_derived(source)?;
        }
        let old = self.host_payload(output, DerivedSlot::Dependencies as u32)?;
        if !old.same_bits(Value::NULL) {
            for i in (0..self.sequence_len(old)?).step_by(2) {
                // Remove one old occurrence, retaining the new subscription.
                let source = self.sequence_get(old, i)?;
                let selector = self.sequence_get(old, i + 1)?;
                self.reactive_remove(source, weak, selector)?;
            }
        }
        self.set_host_payload(output, DerivedSlot::Dependencies as u32, dependencies)?;
        let property = PropertyId {
            host: output,
            field: 0,
        };
        if !self.host_property(property)?.same_bits(result) {
            self.publish_host_property(property, result)?;
        }
        self.end_derived(output)
    }

    pub(crate) fn invalidate_reactive(
        &mut self,
        value: Value,
        field: Option<u32>,
    ) -> Result<(), HeapError> {
        let key = self.key(value)?;
        let source = self.object(key)?.property_owner.unwrap_or(key);
        if let Some(output) = self.reactive_capture
            && let dependencies = self.host_payload(output, DerivedSlot::Candidate as u32)?
            && !dependencies.same_bits(Value::NULL)
        {
            let source_value = Value::object(self.id, source.data().as_ffi());
            for i in (0..self.sequence_len(dependencies)?).step_by(2) {
                let selector = self.sequence_get(dependencies, i + 1)?.as_int().unwrap();
                if self.sequence_get(dependencies, i)?.same_bits(source_value)
                    && (selector == -1 || field.is_none_or(|f| selector == i64::from(f)))
                {
                    self.enqueue_derived(self.key(output)?);
                    break;
                }
            }
        }
        let subscribers = self.object(source)?.reactive_subscribers;
        if subscribers.same_bits(Value::NULL) {
            return Ok(());
        }
        let mut i = 0;
        while i < self.sequence_len(subscribers)? {
            let weak = self.sequence_get(subscribers, i)?;
            let selector = self.sequence_get(subscribers, i + 1)?;
            let Some(output) = self.subscriber_target(weak)? else {
                let owner = Value::object(self.id, source.data().as_ffi());
                self.reactive_remove(owner, weak, selector)?;
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
            for i in (0..self.sequence_len(dependencies)?).step_by(2) {
                self.reactive_remove(
                    self.sequence_get(dependencies, i)?,
                    weak,
                    self.sequence_get(dependencies, i + 1)?,
                )?;
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
        let mut work = crate::root_ranges::ChargedVec::new(&self.usage, 8)?;
        work.data.push(output);
        while let Some(output) = work.data.pop() {
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
            let needed = work.data.len() + len as usize / 2;
            if needed > work.data.capacity() {
                let mut larger = crate::root_ranges::ChargedVec::new(
                    &self.usage,
                    needed.max(work.data.capacity() * 2),
                )?;
                larger.data.append(&mut work.data);
                work = larger;
            }
            for index in (0..len).step_by(2) {
                work.data.push(self.sequence_get(dependencies, index)?);
            }
            self.disconnect_derived(output)?;
            self.set_host_payload(output, DerivedSlot::Suspended as u32, Value::bool(true))?;
        }
        Ok(())
    }

    /// Script-visible service collections are observed but owned by their host.
    pub fn observe_readonly_collection(&mut self, value: Value) -> Result<(), HeapError> {
        let key = self.key(value)?;
        let object = self.objects.get_mut(key).unwrap();
        object.model = true;
        object.readonly_collection = true;
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

    /// Check committed dependencies without running or dequeuing observers.
    /// `evaluating` covers a scheduler's claimed output before begin_derived.
    /// The reusable, accounted workspace visits each dependency once; callers
    /// should use this at a publication boundary, not poll every derived value.
    pub fn derived_is_current(
        &mut self,
        output: Value,
        evaluating: Option<Value>,
    ) -> Result<bool, HeapError> {
        self.property_work.clear();
        self.work_push(PropertyId {
            host: output,
            field: u32::MAX,
        })?;
        let mut cursor = 0;
        while cursor < self.property_work.len() {
            let mut current = self.property_work[cursor];
            cursor += 1;
            if current.field != u32::MAX {
                current = self.property_source(current)?.0;
            }
            let object = self.object(self.key(current.host)?)?;
            if !object.derived {
                continue;
            }
            if object.derived_dirty
                || evaluating.is_some_and(|v| v.same_bits(current.host))
                || self
                    .host_payload(current.host, DerivedSlot::Candidate as u32)?
                    .tag()
                    != crate::ValueTag::Null
            {
                return Ok(false);
            }
            // An untracked component factory also owns an unfinished capture.
            let mut active = self.reactive_capture;
            while let Some(value) = active {
                if value.same_bits(current.host) {
                    return Ok(false);
                }
                let parent = self.host_payload(value, DerivedSlot::Parent as u32)?;
                active = (parent.tag() != crate::ValueTag::Null).then_some(parent);
            }
            let dependencies = self.host_payload(current.host, DerivedSlot::Dependencies as u32)?;
            if dependencies.tag() == crate::ValueTag::Null {
                continue;
            }
            for i in (0..self.sequence_len(dependencies)?).step_by(2) {
                let mut next = PropertyId {
                    host: self.sequence_get(dependencies, i)?,
                    field: self.sequence_get(dependencies, i + 1)?.as_int().unwrap() as u32,
                };
                if next.field != u32::MAX {
                    next = self.property_source(next)?.0;
                }
                // Plain model/collection writes already mark their direct
                // observer dirty. Only derived ancestors need further visits.
                if !self.object(self.key(next.host)?)?.derived {
                    continue;
                }
                next.field = u32::MAX;
                if !self
                    .property_work
                    .iter()
                    .any(|p| p.host.same_bits(next.host) && p.field == next.field)
                {
                    self.work_push(next)?;
                }
            }
        }
        Ok(true)
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
        assert!(!heap.derived_is_current(output, None).unwrap());
        assert!(heap.next_derived().unwrap().unwrap().same_bits(output));
        heap.begin_derived(output).unwrap();
        heap.observe_read(upstream, Some(0)).unwrap();
        heap.commit_derived(output, Value::int(1)).unwrap();
        assert!(heap.derived_is_current(output, None).unwrap());
        assert!(heap.next_derived().unwrap().is_none());
        heap.begin_untracked(output).unwrap();
        assert!(!heap.derived_is_current(output, None).unwrap());
        heap.end_derived(output).unwrap();
        assert!(heap.derived_is_current(output, None).unwrap());
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
