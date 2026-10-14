//! Observable data uses ordinary collection identities and the shared property graph.
use super::*;

#[derive(Clone, Copy)]
#[repr(u32)]
enum ModelSlot {
    Properties,
    Versions,
}

impl Heap {
    pub fn is_model_table(&self, value: Value) -> Result<bool, HeapError> {
        let object = self.object(self.key(value)?)?;
        Ok(object.kind == ObjectKind::Table && object.model)
    }

    /// Resolve a stable property container without changing ordinary table semantics.
    pub fn property_fields(&self, value: Value) -> Result<Value, HeapError> {
        if self.is_model_table(value)? {
            Ok(value)
        } else {
            self.host_fields(value)
        }
    }

    fn model_metadata(&mut self, value: Value) -> Result<Value, HeapError> {
        let key = self.key(value)?;
        let mut metadata = self.object(key)?.model_metadata;
        if metadata.same_bits(Value::NULL) {
            metadata = self.alloc_array(&[Value::NULL, Value::NULL])?;
            self.barrier(metadata)?;
            self.objects.get_mut(key).unwrap().model_metadata = metadata;
        }
        Ok(metadata)
    }

    fn model_slot(&self, value: Value, slot: ModelSlot) -> Result<Value, HeapError> {
        let metadata = self.object(self.key(value)?)?.model_metadata;
        if metadata.same_bits(Value::NULL) {
            Ok(Value::NULL)
        } else {
            self.sequence_get(metadata, slot as u32)
        }
    }

    pub(super) fn property_metadata_array(&self, value: Value) -> Result<Value, HeapError> {
        if self.is_model_table(value)? {
            self.model_slot(value, ModelSlot::Properties)
        } else {
            self.get_field(value, 3)
        }
    }

    pub(super) fn set_property_metadata_array(
        &mut self,
        value: Value,
        metadata: Value,
    ) -> Result<(), HeapError> {
        if self.is_model_table(value)? {
            let storage = self.model_metadata(value)?;
            self.set_field(storage, ModelSlot::Properties as u32, metadata)
        } else {
            self.set_field(value, 3, metadata)
        }
    }

    pub(super) fn property_versions(&self, value: Value) -> Result<Value, HeapError> {
        if self.is_model_table(value)? {
            self.model_slot(value, ModelSlot::Versions)
        } else {
            self.get_field(value, 2)
        }
    }

    pub(super) fn set_property_versions(
        &mut self,
        value: Value,
        versions: Value,
    ) -> Result<(), HeapError> {
        if self.is_model_table(value)? {
            let storage = self.model_metadata(value)?;
            self.set_field(storage, ModelSlot::Versions as u32, versions)
        } else {
            self.set_field(value, 2, versions)
        }
    }

    pub(super) fn reset_model_properties(&mut self, value: Value) -> Result<(), HeapError> {
        let key = self.key(value)?;
        let object = self.objects.get_mut(key).unwrap();
        object.property_generation = object
            .property_generation
            .checked_add(1)
            .ok_or(HeapError::IdentityExhausted)?;
        object.observable_fields = 0;
        object.observable_writable = 0;
        object.model_metadata = Value::NULL;
        Ok(())
    }

    /// A fresh copy is observed only as it is finalized; no author reference can
    /// see the incomplete copy. Existing shared objects are adopted on assignment.
    pub(crate) fn observe_model_object(&mut self, value: Value) -> Result<(), HeapError> {
        let key = self.key(value)?;
        let object = self.objects.get_mut(key).unwrap();
        object.model = true;
        if object.kind == ObjectKind::Table {
            object.property_owner = Some(key);
            object.observable_fields = object.payload.len() / 2;
            object.observable_writable = object.observable_fields;
        }
        Ok(())
    }

    pub(super) fn adopt_model_value(
        &mut self,
        owner: Value,
        value: Value,
    ) -> Result<(), HeapError> {
        self.adopt_model_value_reserving(owner, value, 0)
    }

    pub(super) fn adopt_model_value_reserving(
        &mut self,
        owner: Value,
        value: Value,
        reserved: u64,
    ) -> Result<(), HeapError> {
        if !self.object(self.key(owner)?)?.model {
            return Ok(());
        }
        self.model_work.clear();
        self.model_walk = self
            .model_walk
            .checked_add(1)
            .ok_or(HeapError::IdentityExhausted)?;
        self.model_enqueue(value)?;
        let mut cursor = 0;
        while cursor < self.model_work.len() {
            let current = self.model_work[cursor];
            let key = self.key(current)?;
            let mut edge = 0;
            while let Some(value) = self.object(key)?.payload.edge(edge) {
                self.model_enqueue(value)?;
                edge += 1;
            }
            cursor += 1;
        }
        // Keep room for the caller's collection chunk before publishing observation.
        self.ensure_bytes(reserved)?;
        for index in 0..self.model_work.len() {
            self.observe_model_object(self.model_work[index])?;
        }
        self.model_work.clear();
        Ok(())
    }

    fn model_enqueue(&mut self, value: Value) -> Result<(), HeapError> {
        if value.tag() != crate::ValueTag::Object {
            return Ok(());
        }
        let object = self.object(self.key(value)?)?;
        if object.model
            || object.model_visit == self.model_walk
            || !matches!(
                object.kind,
                ObjectKind::Table
                    | ObjectKind::Array
                    | ObjectKind::Tuple
                    | ObjectKind::TextBuffer
                    | ObjectKind::ByteBuffer
            )
        {
            return Ok(());
        }
        if self.model_work.len() == self.model_work.capacity() {
            let old = self.model_work.capacity();
            let new = (old * 2).max(8);
            self.ensure_bytes(((new - old) * size_of::<Value>()) as u64)?;
            self.model_work
                .try_reserve_exact(new - old)
                .map_err(|_| HeapError::AllocationFailure)?;
            let bytes = ((self.model_work.capacity() - old) * size_of::<Value>()) as u64;
            self.usage.charge(bytes);
            self.reserved += bytes;
        }
        let key = self.key(value)?;
        self.objects.get_mut(key).unwrap().model_visit = self.model_walk;
        self.model_work.push(value);
        Ok(())
    }

    pub(super) fn adopt_model_contents(
        &mut self,
        owner: Value,
        prepared: Value,
    ) -> Result<(), HeapError> {
        self.adopt_model_value(owner, prepared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_updates_reuse_the_property_graph_without_allocating() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let model = heap.alloc_table().unwrap();
        heap.table_store(model, 0, Value::int(0), Value::int(0))
            .unwrap();
        heap.observe_model_object(model).unwrap();
        let reference = heap
            .alloc_property_ref(
                PropertyId {
                    host: model,
                    field: 0,
                },
                true,
            )
            .unwrap();
        let fields = heap.alloc_table().unwrap();
        heap.table_store(fields, 0, Value::int(0), Value::int(0))
            .unwrap();
        let view = heap.alloc_host(fields, &[], None).unwrap();
        heap.observe_host_properties(view, 1, 0).unwrap();
        heap.bind_host_property(
            PropertyId {
                host: view,
                field: 0,
            },
            reference,
        )
        .unwrap();
        let root = heap.add_root(RootOwner(0), RootKind::Host, view).unwrap();
        heap.begin_collection().unwrap();
        while heap.phase() != GcPhase::Idle {
            heap.gc_step(crate::GcBudget::default()).unwrap();
        }
        let before = heap.stats();
        let started = std::time::Instant::now();
        for i in 0..2_000 {
            heap.table_store(model, 0, Value::int(0), Value::int(i))
                .unwrap();
        }
        println!(
            "2000 model-to-view updates: {:?}; no heap growth",
            started.elapsed()
        );
        assert_eq!(heap.table_entry(fields, 0).unwrap().1.as_int(), Some(1999));
        assert_eq!(heap.stats().chunk_allocations, before.chunk_allocations);
        assert_eq!(heap.stats().charged_bytes, before.charged_bytes);
        heap.release_root(root).unwrap();
        heap.begin_collection().unwrap();
        while heap.phase() != GcPhase::Idle {
            heap.gc_step(crate::GcBudget::default()).unwrap();
        }
        assert_eq!(
            heap.stats().live_objects,
            0,
            "reverse subscriptions must remain weak"
        );
    }
}
