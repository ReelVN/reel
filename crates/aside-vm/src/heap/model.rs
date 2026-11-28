//! Observable data uses ordinary collection identities and the shared property graph.
use super::*;
mod adoption;
mod table;
pub(in crate::heap) use adoption::ModelAdoptionProgress;

#[derive(Clone, Copy)]
#[repr(u32)]
enum ModelSlot {
    Properties,
    Versions,
    Fields,
}

impl Heap {
    pub(crate) fn is_model_cell(&self, value: Value) -> Result<bool, HeapError> {
        let object = self.object(self.key(value)?)?;
        Ok(object.kind == ObjectKind::Cell && object.model)
    }

    fn uses_model_properties(&self, value: Value) -> Result<bool, HeapError> {
        let object = self.object(self.key(value)?)?;
        Ok(object.model && matches!(object.kind, ObjectKind::Table | ObjectKind::Cell))
    }
    pub fn is_model_table(&self, value: Value) -> Result<bool, HeapError> {
        let object = self.object(self.key(value)?)?;
        Ok(object.kind == ObjectKind::Table && object.model)
    }

    /// Resolve a stable property container without changing ordinary table semantics.
    pub fn property_fields(&self, value: Value) -> Result<Value, HeapError> {
        if self.uses_model_properties(value)? {
            Ok(value)
        } else {
            self.host_fields(value)
        }
    }

    fn model_metadata(&mut self, value: Value) -> Result<Value, HeapError> {
        let key = self.key(value)?;
        let mut metadata = self.object(key)?.model_metadata;
        if metadata.same_bits(Value::NULL) {
            metadata = self.alloc_array(&[Value::NULL; 3])?;
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

    pub(in crate::heap) fn property_storage_len(&self, host: Value) -> Result<u32, HeapError> {
        if self.is_model_table(host)? {
            self.table_len(host)
        } else {
            Ok(self.object(self.key(host)?)?.observable_fields)
        }
    }

    pub(super) fn property_metadata_array(&self, value: Value) -> Result<Value, HeapError> {
        if self.uses_model_properties(value)? {
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
        if self.uses_model_properties(value)? {
            let storage = self.model_metadata(value)?;
            self.set_field(storage, ModelSlot::Properties as u32, metadata)
        } else {
            self.set_field(value, 3, metadata)
        }
    }

    pub(super) fn property_versions(&self, value: Value) -> Result<Value, HeapError> {
        if self.uses_model_properties(value)? {
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
        if self.uses_model_properties(value)? {
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
        self.mutation_revision = self.mutation_revision.wrapping_add(1);
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
        } else if let Payload::Cell(cell) = &object.payload {
            object.property_owner = Some(key);
            object.observable_fields = 1;
            object.observable_writable = u32::from(cell.contract.mutable);
        }
        self.mutation_revision = self.mutation_revision.wrapping_add(1);
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
            if self.object(self.key(current)?)?.kind == ObjectKind::Array {
                self.adopt_array_edges(current)?;
                cursor += 1;
                continue;
            }
            let mut edge = 0;
            while let Some(value) = self.model_data_edge(current, edge)? {
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

    fn adopt_array_edges(&mut self, array: Value) -> Result<(), HeapError> {
        let mut offset = 0;
        let mut previous = Value::NULL;
        let mut children = [Value::NULL; crate::payload::VALUE_CHUNK_LEN];
        loop {
            let segment = self.array_segment(array, offset)?;
            if segment.is_empty() {
                return Ok(());
            }
            offset += segment.len() as u32;
            let mut count = 0;
            for &value in segment {
                if value.tag() == crate::ValueTag::Object && !value.same_bits(previous) {
                    children[count] = value;
                    count += 1;
                    previous = value;
                }
            }
            for &child in &children[..count] {
                self.model_enqueue(child)?;
            }
        }
    }

    pub(super) fn model_candidate(&self, value: Value) -> Result<bool, HeapError> {
        if value.tag() != crate::ValueTag::Object {
            return Ok(false);
        }
        let object = self.object(self.key(value)?)?;
        if object.model
            || !matches!(
                object.kind,
                ObjectKind::Table
                    | ObjectKind::Array
                    | ObjectKind::Tuple
                    | ObjectKind::TextBuffer
                    | ObjectKind::ByteBuffer
                    | ObjectKind::Instance
                    | ObjectKind::Cell
            )
        {
            return Ok(false);
        }
        if object.kind == ObjectKind::Instance {
            self.instance_data(value)?;
        }
        Ok(true)
    }

    fn model_data_edge(&self, value: Value, edge: u32) -> Result<Option<Value>, HeapError> {
        let object = self.object(self.key(value)?)?;
        if object.kind == ObjectKind::Instance {
            // Class layouts contain methods/statics and are shared, not data edges.
            Ok(if edge == 0 {
                Some(self.instance_data(value)?.1)
            } else {
                None
            })
        } else {
            Ok(object.payload.edge(edge))
        }
    }

    fn model_enqueue(&mut self, value: Value) -> Result<(), HeapError> {
        if !self.model_candidate(value)?
            || self.object(self.key(value)?)?.model_visit == self.model_walk
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
    fn adopting_an_array_reaches_nested_objects_across_storage_blocks() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let owner = heap.alloc_array(&[]).unwrap();
        heap.observe_model_object(owner).unwrap();
        let child = heap.alloc_table().unwrap();
        let nested = heap.alloc_array(&[child]).unwrap();
        let array = heap.alloc_array(&[Value::int(0); 140]).unwrap();
        heap.set_field(array, 31, nested).unwrap();
        heap.set_field(array, 32, nested).unwrap();
        heap.set_field(array, 100, array).unwrap();
        heap.set_field(array, 139, child).unwrap();
        heap.array_push(owner, array).unwrap();
        for value in [array, nested, child] {
            assert!(heap.object(heap.key(value).unwrap()).unwrap().model);
        }
        assert!(heap.sequence_get(array, 100).unwrap().same_bits(array));
    }

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
        let cell = heap
            .alloc_language_cell(crate::CellContract::untyped_mutable())
            .unwrap();
        heap.cell_initialize(cell, Value::int(0)).unwrap();
        heap.observe_model_object(cell).unwrap();
        let cell_reference = heap
            .alloc_property_ref(
                PropertyId {
                    host: cell,
                    field: 0,
                },
                true,
            )
            .unwrap();
        let cell_fields = heap.alloc_table().unwrap();
        heap.table_store(cell_fields, 0, Value::int(0), Value::int(0))
            .unwrap();
        let cell_view = heap.alloc_host(cell_fields, &[], None).unwrap();
        heap.observe_host_properties(cell_view, 1, 0).unwrap();
        heap.bind_host_property(
            PropertyId {
                host: cell_view,
                field: 0,
            },
            cell_reference,
        )
        .unwrap();
        let cell_root = heap
            .add_root(RootOwner(0), RootKind::Host, cell_view)
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
            heap.cell_write(cell, Value::int(i)).unwrap();
        }
        println!(
            "2000 table and class-field model-to-view updates: {:?}; no heap growth",
            started.elapsed()
        );
        assert_eq!(heap.table_entry(fields, 0).unwrap().1.as_int(), Some(1999));
        assert_eq!(
            heap.table_entry(cell_fields, 0).unwrap().1.as_int(),
            Some(1999)
        );
        assert_eq!(heap.stats().chunk_allocations, before.chunk_allocations);
        assert_eq!(heap.stats().charged_bytes, before.charged_bytes);
        heap.release_root(root).unwrap();
        heap.release_root(cell_root).unwrap();
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
