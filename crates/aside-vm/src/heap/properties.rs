//! Stable host properties. Forward connections are traced; subscribers are weak.
//! Cached values are committed only after validating the entire affected graph.
use super::*;
mod batch;
mod cursor;
mod source;
pub(super) use batch::{PreparedBinding, PreparedProperty};
pub use batch::{PropertyRead, PropertyWrites};
pub use cursor::{PropertyBatch, PropertyBatchProgress};
pub(in crate::heap) use source::{SOURCE_WORK, SourceWalkProgress};

#[derive(Clone, Copy, Debug)]
pub struct PropertyId {
    pub host: Value,
    pub field: u32,
}

impl PropertyId {
    fn same(self, other: Self) -> bool {
        self.host.same_bits(other.host) && self.field == other.field
    }
}

#[derive(Clone, Copy)]
#[repr(u32)]
enum Slot {
    Binding,
    Subscribers,
    Claim,
    RequiresWrite,
    Proposed,
}

impl Heap {
    /// Register a writable prefix followed by read-only observable outputs. Other fields
    /// (for example methods) remain ordinary, non-observable host properties.
    pub fn observe_host_properties(
        &mut self,
        host: Value,
        writable: u32,
        readonly: u32,
    ) -> Result<(), HeapError> {
        let count = writable
            .checked_add(readonly)
            .ok_or(HeapError::OutOfBounds)?;
        let fields = self.host_fields(host)?;
        if count > self.table_len(fields)? {
            return Err(HeapError::OutOfBounds);
        }
        let owner = self.key(host)?;
        let key = self.key(host)?;
        let object = self.objects.get_mut(key).unwrap();
        object.observable_fields = count;
        object.observable_writable = writable;
        let key = self.key(fields)?;
        self.objects.get_mut(key).unwrap().property_owner = Some(owner);
        self.mutation_revision = self.mutation_revision.wrapping_add(1);
        Ok(())
    }

    fn property_access(&self, property: PropertyId) -> Result<bool, HeapError> {
        self.property_fields(property.host)?;
        let object = self.object(self.key(property.host)?)?;
        if property.field >= object.observable_fields {
            return Err(HeapError::WrongKind);
        }
        self.model_position(property)?;
        Ok(property.field < object.observable_writable)
    }

    pub fn is_property_ref(&self, value: Value) -> bool {
        value.tag() == crate::ValueTag::Object
            && self
                .info(value)
                .is_ok_and(|info| info.kind == ObjectKind::PropertyRef)
    }

    pub fn alloc_property_ref(
        &mut self,
        property: PropertyId,
        writable: bool,
    ) -> Result<Value, HeapError> {
        let writable = writable && self.property_access(property)?;
        self.alloc_record(
            ObjectKind::PropertyRef,
            &[
                property.host,
                Value::int(i64::from(property.field)),
                Value::bool(writable),
                Value::int(self.object(self.key(property.host)?)?.property_generation as i64),
            ],
        )
    }

    pub fn property_ref(&self, reference: Value) -> Result<(PropertyId, bool), HeapError> {
        if !self.is_property_ref(reference) {
            return Err(HeapError::WrongKind);
        }
        let property = PropertyId {
            host: self.get_field(reference, 0)?,
            field: self.get_field(reference, 1)?.as_int().unwrap() as u32,
        };
        self.property_access(property)?;
        if self.get_field(reference, 3)?.as_int()
            != Some(self.object(self.key(property.host)?)?.property_generation as i64)
        {
            return Err(HeapError::RetiredHost);
        }
        Ok((property, self.get_field(reference, 2)?.as_bool().unwrap()))
    }

    /// Resolve a constructor argument without discarding the original reference.
    pub fn property_argument(&self, value: Value) -> Result<Value, HeapError> {
        if self.is_property_ref(value) {
            let (property, _) = self.property_ref(value)?;
            self.host_property(property)
        } else {
            Ok(value)
        }
    }

    pub fn host_property(&self, property: PropertyId) -> Result<Value, HeapError> {
        if self.is_model_cell(property.host)? {
            self.property_access(property)?;
            return self.cell_read(property.host);
        }
        Ok(self
            .table_entry(
                self.property_fields(property.host)?,
                self.model_position(property)?,
            )?
            .1)
    }

    fn property_metadata(&self, property: PropertyId) -> Result<Value, HeapError> {
        self.property_fields(property.host)?;
        let metadata = self.property_metadata_array(property.host)?;
        if metadata.same_bits(Value::NULL) {
            return Ok(Value::NULL);
        }
        if property.field >= self.sequence_len(metadata)? {
            return Ok(Value::NULL);
        }
        self.sequence_get(metadata, property.field)
    }

    fn ensure_property_metadata(&mut self, property: PropertyId) -> Result<Value, HeapError> {
        self.property_access(property)?;
        let mut metadata = self.property_metadata_array(property.host)?;
        if metadata.same_bits(Value::NULL) {
            metadata = self.alloc_array(&[])?;
            for _ in 0..self.object(self.key(property.host)?)?.observable_fields {
                self.property_traversal_work
                    .set(self.property_traversal_work.get() + 4);
                self.array_push(metadata, Value::NULL)?;
            }
            self.set_property_metadata_array(property.host, metadata)?;
        }
        while self.sequence_len(metadata)? <= property.field {
            self.property_traversal_work
                .set(self.property_traversal_work.get() + 4);
            self.array_push(metadata, Value::NULL)?;
        }
        let mut entry = self.sequence_get(metadata, property.field)?;
        if entry.same_bits(Value::NULL) {
            entry = self.alloc_array(&[Value::NULL; 5])?;
            self.set_field(metadata, property.field, entry)?;
        }
        Ok(entry)
    }

    fn property_binding(&self, property: PropertyId) -> Result<Value, HeapError> {
        let metadata = self.property_metadata(property)?;
        if metadata.same_bits(Value::NULL) {
            Ok(Value::NULL)
        } else {
            self.sequence_get(metadata, Slot::Binding as u32)
        }
    }

    pub fn property_source(
        &self,
        mut property: PropertyId,
    ) -> Result<(PropertyId, bool), HeapError> {
        let mut writable = self.property_access(property)?;
        loop {
            let binding = self.property_binding(property)?;
            if binding.same_bits(Value::NULL) {
                return Ok((property, writable));
            }
            let (source, permission) = self.property_ref(binding)?;
            writable &= permission && self.property_access(source)?;
            property = source;
        }
    }

    pub(super) fn subscribers(&self, property: PropertyId) -> Result<Value, HeapError> {
        let entry = self.property_metadata(property)?;
        if entry.same_bits(Value::NULL) {
            Ok(Value::NULL)
        } else {
            self.sequence_get(entry, Slot::Subscribers as u32)
        }
    }

    pub(super) fn work_push(&mut self, property: PropertyId) -> Result<(), HeapError> {
        if self.property_work.len() == self.property_work.capacity() {
            let old = self.property_work.capacity();
            let new = (old * 2).max(8);
            let bytes = ((new - old) * std::mem::size_of::<PropertyId>()) as u64;
            self.ensure_bytes(bytes)?;
            self.property_work
                .try_reserve_exact(new - old)
                .map_err(|_| HeapError::AllocationFailure)?;
            let bytes =
                ((self.property_work.capacity() - old) * std::mem::size_of::<PropertyId>()) as u64;
            self.usage.charge(bytes);
            self.reserved += bytes;
        }
        self.property_work.push(property);
        Ok(())
    }

    // Internal subscription traversal must not shade weak targets. Upgrading
    // during every marking cycle would keep abandoned views alive indefinitely.
    // No collection step runs inside a property transaction and these values
    // never escape it; live unmarked targets still receive their cached update.
    pub(super) fn subscriber_target(&self, reference: Value) -> Result<Option<Value>, HeapError> {
        self.peek_weakref(reference)
    }

    fn collect_properties(&mut self, property: PropertyId) -> Result<(), HeapError> {
        self.property_work.clear();
        self.work_push(property)?;
        let mut cursor = 0;
        while cursor < self.property_work.len() {
            let source = self.property_work[cursor];
            cursor += 1;
            let subscribers = self.subscribers(source)?;
            if subscribers.same_bits(Value::NULL) {
                continue;
            }
            let mut index = 0;
            while index < self.sequence_len(subscribers)? {
                let weak = self.sequence_get(subscribers, index)?;
                let field = self.sequence_get(subscribers, index + 1)?.as_int().unwrap() as u32;
                if let Some(host) = self.subscriber_target(weak)?
                    && self.property_fields(host).is_ok()
                {
                    self.work_push(PropertyId { host, field })?;
                    index += 2;
                } else {
                    self.remove_subscriber(subscribers, index)?;
                }
            }
        }
        Ok(())
    }

    fn remove_subscriber(&mut self, list: Value, index: u32) -> Result<(), HeapError> {
        let last = self.sequence_len(list)? - 2;
        if index != last {
            for offset in 0..2 {
                self.set_field(
                    list,
                    index + offset,
                    self.sequence_get(list, last + offset)?,
                )?;
            }
        }
        self.array_pop(list)?;
        self.array_pop(list)?;
        Ok(())
    }

    pub(crate) fn proposed_property(&self, table: Value, field: u32) -> Option<Value> {
        let value = self.property_proposal?;
        let owner = self.object(self.key(table).ok()?).ok()?.property_owner?;
        let host = Value::object(self.id, owner.data().as_ffi());
        let field = self.model_field(host, field).ok()?;
        if self
            .property_work
            .first()
            .is_some_and(|p| p.host.same_bits(host) && p.field == field)
        {
            return Some(value);
        }
        let metadata = self.property_metadata(PropertyId { host, field }).ok()?;
        (!metadata.same_bits(Value::NULL)
            && self
                .sequence_get(metadata, Slot::Proposed as u32)
                .ok()?
                .as_bool()
                == Some(true))
        .then_some(value)
    }

    fn validate_properties(&mut self, value: Value) -> Result<(), HeapError> {
        for i in 0..self.property_work.len() {
            let metadata = self.property_metadata(self.property_work[i])?;
            if !metadata.same_bits(Value::NULL) {
                self.set_field(metadata, Slot::Proposed as u32, Value::bool(true))?;
            }
        }
        self.property_proposal = Some(value);
        let result = self
            .property_work
            .iter()
            .try_for_each(|p| self.validate_host_property(p.host, p.field, value));
        self.property_proposal = None;
        for i in 0..self.property_work.len() {
            let metadata = self.property_metadata(self.property_work[i])?;
            if !metadata.same_bits(Value::NULL) {
                self.set_field(metadata, Slot::Proposed as u32, Value::bool(false))?;
            }
        }
        result
    }

    fn prepare_properties(&mut self, value: Value, script: bool) -> Result<(), HeapError> {
        for i in 0..self.property_work.len() {
            let host = self.property_work[i].host;
            if script && !self.property_versions(host)?.same_bits(Value::NULL) {
                self.track_host_writes(host)?;
            }
            self.adopt_model_value(host, value)?;
        }
        Ok(())
    }

    fn commit_properties(&mut self, value: Value, script: bool) -> Result<(), HeapError> {
        self.prepare_properties(value, script)?;
        for i in 0..self.property_work.len() {
            let property = self.property_work[i];
            self.commit_property_value(property, value, script)?;
        }
        self.property_work.clear();
        Ok(())
    }

    fn commit_property_value(
        &mut self,
        property: PropertyId,
        value: Value,
        script: bool,
    ) -> Result<(), HeapError> {
        if self.is_model_cell(property.host)? {
            if !self.cell_read(property.host)?.same_bits(value) {
                self.store_cell_value(property.host, value)?;
            }
        } else {
            let fields = self.property_fields(property.host)?;
            let position = self.model_position(property)?;
            let (_, old) = self.table_entry(fields, position)?;
            if !old.same_bits(value) {
                self.store_prepared_property_value(fields, position, value)?;
            }
        }
        if script {
            self.record_host_write(property.host, property.field)?;
        }
        Ok(())
    }

    /// Publish an authoritative host value, including read-only service data.
    /// Script/control/animation writers use `write_host_property` instead.
    pub fn publish_host_property(
        &mut self,
        property: PropertyId,
        value: Value,
    ) -> Result<(), HeapError> {
        let source = self.property_source(property)?.0;
        self.collect_properties(source)?;
        self.validate_properties(value)?;
        self.commit_properties(value, false)
    }

    /// Preflight an animation endpoint against every connected constraint.
    pub fn validate_property_write(
        &mut self,
        property: PropertyId,
        value: Value,
    ) -> Result<(), HeapError> {
        let (source, writable) = self.property_source(property)?;
        if !writable {
            return Err(HeapError::ReadOnly);
        }
        self.collect_properties(source)?;
        self.validate_properties(value)
    }

    /// Script and control writes preserve connections. Animation producers pass
    /// `script = false`, so their own samples do not cancel their reservation.
    pub fn write_host_property(
        &mut self,
        property: PropertyId,
        value: Value,
        script: bool,
    ) -> Result<(), HeapError> {
        if self.is_property_ref(value) {
            return self.bind_host_property(property, value);
        }
        // Non-observable host fields still use their original validation contract.
        if property.field >= self.object(self.key(property.host)?)?.observable_fields {
            self.validate_host_property(property.host, property.field, value)?;
            let fields = self.property_fields(property.host)?;
            let key = self.table_entry(fields, property.field)?.0;
            self.store_table_entry(fields, property.field, key, value)?;
            return self.record_host_write(property.host, property.field);
        }
        let (source, writable) = self.property_source(property)?;
        if !writable {
            return Err(HeapError::ReadOnly);
        }
        self.collect_properties(source)?;
        self.validate_properties(value)?;
        self.commit_properties(value, script)
    }

    pub fn bind_host_property(
        &mut self,
        target: PropertyId,
        reference: Value,
    ) -> Result<(), HeapError> {
        if !self.property_access(target)? {
            return Err(HeapError::ReadOnly);
        }
        let (source, permission) = self.property_ref(reference)?;
        let writable = permission && self.property_source(source)?.1;
        let mut cursor = source;
        loop {
            if cursor.same(target) {
                return Err(HeapError::BindingCycle);
            }
            let binding = self.property_binding(cursor)?;
            if binding.same_bits(Value::NULL) {
                break;
            }
            cursor = self.property_ref(binding)?.0;
        }
        self.check_derived_cycle(target, source)?;
        let value = self.host_property(source)?;
        self.collect_properties(target)?;
        if !writable {
            for property in &self.property_work {
                let metadata = self.property_metadata(*property)?;
                if !metadata.same_bits(Value::NULL)
                    && self
                        .sequence_get(metadata, Slot::RequiresWrite as u32)?
                        .as_bool()
                        == Some(true)
                {
                    return Err(HeapError::ReadOnly);
                }
            }
        }
        self.validate_properties(value)?;
        self.prepare_properties(value, true)?;
        let target_meta = self.ensure_property_metadata(target)?;
        let source_meta = self.ensure_property_metadata(source)?;
        let mut list = self.sequence_get(source_meta, Slot::Subscribers as u32)?;
        if list.same_bits(Value::NULL) {
            list = self.alloc_array(&[])?;
            self.set_field(source_meta, Slot::Subscribers as u32, list)?;
        }
        let weak = self.alloc_weakref(target.host)?;
        // Append before disconnecting the old edge; all allocations precede
        // publication, so allocation failure cannot destroy the old connection.
        self.array_push(list, weak)?;
        if let Err(error) = self.array_push(list, Value::int(i64::from(target.field))) {
            self.array_pop(list)?;
            return Err(error);
        }
        self.detach_property(target)?;
        self.binding_changed()?;
        self.set_field(target_meta, Slot::Binding as u32, reference)?;
        self.reconnect_derived(source.host)?;
        self.commit_properties(value, true)
    }

    fn binding_changed(&mut self) -> Result<(), HeapError> {
        self.binding_revision = self
            .binding_revision
            .checked_add(1)
            .ok_or(HeapError::IdentityExhausted)?;
        Ok(())
    }

    fn detach_property(&mut self, target: PropertyId) -> Result<(), HeapError> {
        let binding = self.property_binding(target)?;
        if binding.same_bits(Value::NULL) {
            return Ok(());
        }
        let (source, _) = self.property_ref(binding)?;
        let list = self.subscribers(source)?;
        for index in (0..self.sequence_len(list)?).step_by(2) {
            self.property_traversal_work
                .set(self.property_traversal_work.get() + 8);
            let weak = self.sequence_get(list, index)?;
            if self
                .subscriber_target(weak)?
                .is_some_and(|host| host.same_bits(target.host))
                && self.sequence_get(list, index + 1)?.as_int() == Some(i64::from(target.field))
            {
                self.remove_subscriber(list, index)?;
                break;
            }
        }
        let entry = self.property_metadata(target)?;
        self.binding_changed()?;
        self.set_field(entry, Slot::Binding as u32, Value::NULL)?;
        self.disconnect_unobserved_derived(source.host)
    }

    pub fn unbind_property(&mut self, reference: Value) -> Result<Value, HeapError> {
        let (target, writable) = self.property_ref(reference)?;
        if !writable {
            return Err(HeapError::ReadOnly);
        }
        let value = self.host_property(target)?;
        self.collect_properties(target)?;
        self.detach_property(target)?;
        self.commit_properties(value, true)?;
        Ok(value)
    }

    /// A control that writes back cannot consume a read-only source.
    pub fn require_writable_property(&mut self, property: PropertyId) -> Result<(), HeapError> {
        if !self.property_source(property)?.1 {
            return Err(HeapError::ReadOnly);
        }
        let entry = self.ensure_property_metadata(property)?;
        self.set_field(entry, Slot::RequiresWrite as u32, Value::bool(true))
    }

    pub fn property_claim(&self, property: PropertyId) -> Result<Value, HeapError> {
        let entry = self.property_metadata(property)?;
        if entry.same_bits(Value::NULL) {
            Ok(Value::NULL)
        } else {
            self.sequence_get(entry, Slot::Claim as u32)
        }
    }

    pub fn set_property_claim(
        &mut self,
        property: PropertyId,
        claim: Value,
    ) -> Result<(), HeapError> {
        let entry = self.ensure_property_metadata(property)?;
        self.set_field(entry, Slot::Claim as u32, claim)
    }

    pub(super) fn disconnect_host_properties(&mut self, host: Value) -> Result<(), HeapError> {
        let count = self.object(self.key(host)?)?.observable_fields;
        for field in 0..count {
            self.disconnect_property(PropertyId { host, field })?;
        }
        Ok(())
    }

    pub(super) fn disconnect_property(&mut self, source: PropertyId) -> Result<(), HeapError> {
        self.detach_property(source)?;
        let list = self.subscribers(source)?;
        while !list.same_bits(Value::NULL) && self.sequence_len(list)? != 0 {
            let weak = self.sequence_get(list, 0)?;
            let field = self.sequence_get(list, 1)?.as_int().unwrap() as u32;
            if let Some(host) = self.subscriber_target(weak)?
                && self.property_fields(host).is_ok()
            {
                let target = PropertyId { host, field };
                let value = self.host_property(target)?;
                self.collect_properties(target)?;
                self.detach_property(target)?;
                self.commit_properties(value, true)?;
            } else {
                self.remove_subscriber(list, 0)?;
            }
        }
        if !self.property_metadata(source)?.same_bits(Value::NULL) {
            let metadata = self.property_metadata_array(source.host)?;
            self.set_field(metadata, source.field, Value::NULL)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(heap: &mut Heap, values: &[Value], writable: bool) -> Value {
        let fields = heap.alloc_table().unwrap();
        for (i, &value) in values.iter().enumerate() {
            heap.table_store(fields, i as u32, Value::int(i as i64), value)
                .unwrap();
        }
        let host = heap.alloc_host(fields, &[], None).unwrap();
        heap.observe_host_properties(
            host,
            if writable { values.len() as u32 } else { 0 },
            if writable { 0 } else { values.len() as u32 },
        )
        .unwrap();
        host
    }
    fn collect(heap: &mut Heap) {
        heap.begin_collection().unwrap();
        while heap.phase() != GcPhase::Idle {
            heap.gc_step(crate::GcBudget::default()).unwrap();
        }
    }

    #[test]
    fn bindings_preserve_permissions_aliases_and_weak_subscriber_lifetimes() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let source = host(&mut heap, &[Value::int(7)], false);
        let target = host(&mut heap, &[Value::int(0)], true);
        let source_id = PropertyId {
            host: source,
            field: 0,
        };
        let target_id = PropertyId {
            host: target,
            field: 0,
        };
        let reference = heap.alloc_property_ref(source_id, true).unwrap();
        heap.bind_host_property(target_id, reference).unwrap();
        assert_eq!(heap.host_property(target_id).unwrap().as_int(), Some(7));
        assert_eq!(
            heap.write_host_property(target_id, Value::int(8), true)
                .unwrap_err(),
            HeapError::ReadOnly
        );
        assert_eq!(
            heap.require_writable_property(target_id).unwrap_err(),
            HeapError::ReadOnly
        );
        heap.publish_host_property(source_id, Value::int(9))
            .unwrap();
        assert_eq!(heap.host_property(target_id).unwrap().as_int(), Some(9));
        let root = heap.add_root(RootOwner(0), RootKind::Host, target).unwrap();
        collect(&mut heap);
        assert!(
            heap.host_fields(source).is_ok(),
            "binding must retain its source"
        );
        heap.release_root(root).unwrap();
        let source_root = heap.add_root(RootOwner(0), RootKind::Host, source).unwrap();
        heap.begin_collection().unwrap();
        while heap.phase() != GcPhase::Idle {
            heap.publish_host_property(source_id, Value::int(10))
                .unwrap();
            heap.gc_step(crate::GcBudget {
                units: 1,
                ..Default::default()
            })
            .unwrap();
        }
        assert!(
            heap.info(target).is_err(),
            "updates during marking cannot resurrect weak subscribers"
        );
        heap.release_root(source_root).unwrap();
        collect(&mut heap);
        assert_eq!(
            heap.stats().live_objects,
            0,
            "weak reverse edges cannot retain an abandoned graph"
        );
        assert_eq!(heap.stats().charged_bytes, heap.stats().reserved_bytes);
    }

    #[test]
    fn candidate_graph_is_atomic_and_propagation_reuses_storage() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let source = host(&mut heap, &[Value::int(1)], true);
        let source = PropertyId {
            host: source,
            field: 0,
        };
        let reference = heap.alloc_property_ref(source, true).unwrap();
        let pair = host(&mut heap, &[Value::int(1), Value::int(1)], true);
        let key = heap.key(pair).unwrap();
        heap.objects.get_mut(key).unwrap().host_validator = Some(|heap, host, _, _| {
            let fields = heap.host_fields(host)?;
            let a = heap.table_entry(fields, 0)?.1.as_int().unwrap();
            let b = heap.table_entry(fields, 1)?.1.as_int().unwrap();
            if a != b {
                Err(HeapError::TypeMismatch)
            } else {
                Ok(())
            }
        });
        for field in 0..2 {
            heap.bind_host_property(PropertyId { host: pair, field }, reference)
                .unwrap();
        }
        let targets = heap.alloc_array(&[pair]).unwrap();
        for _ in 0..127 {
            let target = host(&mut heap, &[Value::int(1)], true);
            heap.bind_host_property(
                PropertyId {
                    host: target,
                    field: 0,
                },
                reference,
            )
            .unwrap();
            heap.array_push(targets, target).unwrap();
        }
        let root = heap
            .add_root(RootOwner(0), RootKind::Host, targets)
            .unwrap();
        // Updating both pair fields must validate against (2,2), never (2,1).
        heap.write_host_property(source, Value::int(2), true)
            .unwrap();
        collect(&mut heap);
        let chunks = heap.stats().chunk_allocations;
        let charged = heap.stats().charged_bytes;
        let started = std::time::Instant::now();
        for i in 0..2000 {
            heap.write_host_property(source, Value::int(i), true)
                .unwrap();
        }
        println!(
            "2000 writes through 129 connected fields: {:?}; zero additional heap chunks",
            started.elapsed()
        );
        assert_eq!(heap.stats().chunk_allocations, chunks);
        assert_eq!(heap.stats().charged_bytes, charged);
        assert_eq!(
            heap.table_entry(heap.host_fields(pair).unwrap(), 1)
                .unwrap()
                .1
                .as_int(),
            Some(1999)
        );
        heap.retire_host(source.host).unwrap();
        heap.write_host_property(
            PropertyId {
                host: pair,
                field: 0,
            },
            Value::int(1999),
            true,
        )
        .unwrap();
        assert_eq!(
            heap.property_ref(reference).unwrap_err(),
            HeapError::RetiredHost
        );
        heap.release_root(root).unwrap();
        collect(&mut heap);
        assert_eq!(heap.stats().live_objects, 0);
    }
}
