//! Model field identities only need indirection after the first slot removal.
use super::*;

impl Heap {
    pub(crate) fn model_field(&self, table: Value, position: u32) -> Result<u32, HeapError> {
        if !self.is_model_table(table)? {
            return Ok(position);
        }
        let fields = self.model_slot(table, ModelSlot::Fields)?;
        if fields.same_bits(Value::NULL) {
            Ok(position)
        } else if position == self.table_len(table)? {
            Ok(self.object(self.key(table)?)?.observable_fields)
        } else {
            Ok(self.sequence_get(fields, position)?.as_int().unwrap() as u32)
        }
    }

    pub(in crate::heap) fn model_position(&self, property: PropertyId) -> Result<u32, HeapError> {
        if !self.is_model_table(property.host)? {
            return Ok(property.field);
        }
        let fields = self.model_slot(property.host, ModelSlot::Fields)?;
        if fields.same_bits(Value::NULL) {
            return Ok(property.field);
        }
        // New identities are appended in increasing order and never reused.
        // Searching only live fields avoids retaining an entry for every past ID.
        let mut begin = 0;
        let mut end = self.sequence_len(fields)?;
        while begin < end {
            let position = begin + (end - begin) / 2;
            let field = self.sequence_get(fields, position)?.as_int().unwrap() as u32;
            match field.cmp(&property.field) {
                std::cmp::Ordering::Less => begin = position + 1,
                std::cmp::Ordering::Equal => return Ok(position),
                std::cmp::Ordering::Greater => end = position,
            }
        }
        Err(HeapError::RetiredHost)
    }

    pub(crate) fn prepare_model_table_remove(
        &mut self,
        table: Value,
        prepared: Value,
    ) -> Result<u32, HeapError> {
        if !self.is_model_table(table)? {
            return Ok(0);
        }
        let metadata = self.model_metadata(prepared)?;
        let fields = self.alloc_array(&[])?;
        self.set_field(metadata, ModelSlot::Fields as u32, fields)?;
        Ok(self.table_len(table)? - 1)
    }

    pub(in crate::heap) fn finish_model_table_remove(
        &mut self,
        table: Value,
        prepared: Value,
        removed: u32,
    ) -> Result<(), HeapError> {
        // Binding and activity metadata may be added between copy slices without
        // changing table contents. Transfer it after the removed field retires.
        let metadata = self.object(self.key(prepared)?)?.model_metadata;
        for slot in [ModelSlot::Properties, ModelSlot::Versions] {
            let array = self.model_slot(table, slot)?;
            if !array.same_bits(Value::NULL) {
                self.remove_model_metadata_position(array, removed)?;
            }
            self.set_field(metadata, slot as u32, array)?;
        }
        Ok(())
    }

    pub(crate) fn model_table_remove_work(
        &self,
        table: Value,
        removed: u32,
    ) -> Result<u64, HeapError> {
        if !self.is_model_table(table)? {
            return Ok(0);
        }
        let mut work = 0;
        for slot in [ModelSlot::Properties, ModelSlot::Versions] {
            let array = self.model_slot(table, slot)?;
            if !array.same_bits(Value::NULL) {
                let len = self.sequence_len(array)?;
                if removed < len {
                    work += u64::from(len - removed - 1) * 2 + 1;
                }
            }
        }
        Ok(work)
    }

    fn remove_model_metadata_position(
        &mut self,
        array: Value,
        removed: u32,
    ) -> Result<(), HeapError> {
        let len = self.sequence_len(array)?;
        if removed >= len {
            return Ok(());
        }
        // These arrays are private. Shade moved edges before shifting them behind
        // an incremental GC cursor; no allocation or creator callback can intervene.
        for position in removed + 1..len {
            self.barrier(self.sequence_get(array, position)?)?;
        }
        let key = self.key(array)?;
        let object = self.objects.get_mut(key).unwrap();
        let Payload::Values(values) = &mut object.payload else {
            unreachable!()
        };
        for position in removed..len - 1 {
            values.set(position, values.get(position + 1).unwrap());
        }
        let (_, freed) = values.pop().unwrap();
        object.charged -= freed;
        object.content_version = object.content_version.wrapping_add(1);
        object.structure_version = object.structure_version.wrapping_add(1);
        self.mutation_revision = self.mutation_revision.wrapping_add(1);
        self.freed(freed);
        Ok(())
    }

    pub(crate) fn model_table_remove_step(
        &mut self,
        table: Value,
        prepared: Value,
        removed: u32,
        index: u32,
    ) -> Result<(), HeapError> {
        if !self.is_model_table(table)? {
            return Ok(());
        }
        if index < self.table_len(table)? - 1 {
            let source = index + u32::from(index >= removed);
            let field = self.model_field(table, source)?;
            self.array_push(
                self.model_slot(prepared, ModelSlot::Fields)?,
                Value::int(i64::from(field)),
            )?;
        }
        Ok(())
    }

    pub(crate) fn model_append_bytes(&self, table: Value) -> Result<u64, HeapError> {
        if !self.is_model_table(table)? {
            return Ok(0);
        }
        let fields = self.model_slot(table, ModelSlot::Fields)?;
        if fields.same_bits(Value::NULL) {
            return Ok(0);
        }
        Ok(Values::growth_bytes(self.sequence_len(fields)?.into(), 1))
    }

    pub(in crate::heap) fn append_model_position(&mut self, table: Value) -> Result<(), HeapError> {
        if !self.is_model_table(table)? {
            return Ok(());
        }
        let fields = self.model_slot(table, ModelSlot::Fields)?;
        if fields.same_bits(Value::NULL) {
            return Ok(());
        }
        let field = self.object(self.key(table)?)?.observable_fields;
        self.array_push(fields, Value::int(i64::from(field)))
    }

    pub(in crate::heap) fn rollback_model_position(
        &mut self,
        table: Value,
    ) -> Result<(), HeapError> {
        let fields = self.model_slot(table, ModelSlot::Fields)?;
        if !fields.same_bits(Value::NULL) {
            self.array_pop(fields)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(heap: &mut Heap, values: &[i64]) -> Value {
        let table = heap.alloc_table().unwrap();
        for (index, &value) in values.iter().enumerate() {
            heap.table_store(
                table,
                index as u32,
                Value::int(index as i64),
                Value::int(value),
            )
            .unwrap();
        }
        heap.observe_model_object(table).unwrap();
        table
    }

    fn reference(heap: &mut Heap, table: Value, position: u32) -> Value {
        heap.alloc_property_ref(
            PropertyId {
                host: table,
                field: heap.model_field(table, position).unwrap(),
            },
            true,
        )
        .unwrap()
    }

    fn prepare_remove(heap: &mut Heap, table: Value, position: u32) -> Value {
        let before = heap.info(table).unwrap();
        let prepared = heap.alloc_table().unwrap();
        let fields = heap.prepare_model_table_remove(table, prepared).unwrap();
        for index in 0..(before.len - 1).max(fields) {
            if index < before.len - 1 {
                let source = index + u32::from(index >= position);
                let (key, value) = heap.table_entry(table, source).unwrap();
                heap.table_store(prepared, index, key, value).unwrap();
            }
            heap.model_table_remove_step(table, prepared, position, index)
                .unwrap();
        }
        prepared
    }

    fn remove(heap: &mut Heap, table: Value, position: u32) -> Value {
        let before = heap.info(table).unwrap();
        let result = heap.table_entry(table, position).unwrap().1;
        let prepared = prepare_remove(heap, table, position);
        heap.commit_table_remove(table, prepared, position).unwrap();
        let after = heap.info(table).unwrap();
        assert_ne!(before.content_version, after.content_version);
        assert_ne!(before.structure_version, after.structure_version);
        result
    }

    #[test]
    fn model_table_remove_preserves_bindings_versions_and_claims_added_between_slices() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let table = model(&mut heap, &[10, 20, 30]);
        let prepared = prepare_remove(&mut heap, table, 0);
        let before = heap.info(table).unwrap();
        let upstream = model(&mut heap, &[20]);
        let reference = reference(&mut heap, upstream, 0);
        let retained = PropertyId {
            host: table,
            field: 1,
        };
        let removed = PropertyId {
            host: table,
            field: 0,
        };
        heap.bind_host_property(retained, reference).unwrap();
        heap.track_host_writes(table).unwrap();
        let claim = heap.alloc_array(&[Value::int(42)]).unwrap();
        heap.set_property_claim(retained, claim).unwrap();
        heap.set_property_claim(removed, claim).unwrap();
        assert_eq!(
            heap.info(table).unwrap().content_version,
            before.content_version
        );
        heap.commit_table_remove(table, prepared, 0).unwrap();
        assert!(heap.property_claim(retained).unwrap().same_bits(claim));
        assert!(heap.property_claim(removed).unwrap().same_bits(Value::NULL));
        assert_eq!(heap.host_write_version(table, 1).unwrap(), 0);
        heap.table_store(upstream, 0, Value::int(0), Value::int(21))
            .unwrap();
        assert_eq!(heap.table_entry(table, 0).unwrap().1.as_int(), Some(21));
        assert_eq!(heap.host_write_version(table, 1).unwrap(), 1);
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
    fn model_table_metadata_stays_bounded_when_fields_are_replaced() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let table = model(&mut heap, &[0; 64]);
        heap.add_root(RootOwner(0), RootKind::Host, table).unwrap();
        heap.track_host_writes(table).unwrap();
        let mut early = None;
        for iteration in 0..2048 {
            remove(&mut heap, table, 0);
            let next = 64 + iteration;
            heap.table_store(table, 63, Value::int(next), Value::int(next))
                .unwrap();
            let property = PropertyId {
                host: table,
                field: heap.model_field(table, 63).unwrap(),
            };
            heap.set_property_claim(property, Value::int(next)).unwrap();
            heap.record_host_write(table, property.field).unwrap();
            if iteration % 64 == 63 {
                collect(&mut heap);
                if early.is_none() {
                    early = Some(heap.stats());
                }
            }
        }
        let early = early.unwrap();
        let after = heap.stats();
        eprintln!("model churn: early={early:?}; after={after:?}");
        assert_eq!(after.live_objects, early.live_objects);
        assert_eq!(after.payload_bytes, early.payload_bytes);
        assert_eq!(heap.table_len(table).unwrap(), 64);
        assert_eq!(heap.model_field(table, 63).unwrap(), 2111);
        assert_eq!(heap.host_write_version(table, 2111).unwrap(), 1);
    }

    #[test]
    fn model_table_remove_preserves_metadata_moved_behind_the_gc_cursor() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let table = model(&mut heap, &[10, 20, 30]);
        heap.add_root(RootOwner(0), RootKind::Host, table).unwrap();
        let retained = PropertyId {
            host: table,
            field: 2,
        };
        let claim = heap.alloc_array(&[Value::int(42)]).unwrap();
        heap.set_property_claim(retained, claim).unwrap();
        let metadata = heap.property_metadata_array(table).unwrap();
        let entry = heap.sequence_get(metadata, 2).unwrap();
        let key = heap.key(metadata).unwrap();
        heap.begin_collection().unwrap();
        while heap.object(key).unwrap().scan < 2 {
            assert_eq!(heap.phase(), GcPhase::Mark);
            heap.gc_step(crate::GcBudget {
                units: 1,
                ..Default::default()
            })
            .unwrap();
        }
        assert_ne!(
            heap.object(heap.key(entry).unwrap()).unwrap().mark,
            heap.gc.cycle
        );
        remove(&mut heap, table, 0);
        while heap.phase() != GcPhase::Idle {
            heap.gc_step(crate::GcBudget {
                units: 1,
                ..Default::default()
            })
            .unwrap();
        }
        assert!(heap.property_claim(retained).unwrap().same_bits(claim));
        assert_eq!(heap.sequence_get(claim, 0).unwrap().as_int(), Some(42));
        assert_eq!(heap.model_field(table, 1).unwrap(), 2);
    }

    #[test]
    fn model_table_remove_retains_binding_sources_targets_and_retires_only_deleted_field() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let table = model(&mut heap, &[10, 20, 30]);
        let downstream = model(&mut heap, &[0, 0, 0]);
        let upstream = model(&mut heap, &[40]);
        let refs: Vec<_> = (0..3)
            .map(|index| reference(&mut heap, table, index))
            .collect();
        for (field, &reference) in refs.iter().enumerate() {
            heap.bind_host_property(
                PropertyId {
                    host: downstream,
                    field: field as u32,
                },
                reference,
            )
            .unwrap();
        }
        let upstream_ref = reference(&mut heap, upstream, 0);
        heap.bind_host_property(
            PropertyId {
                host: table,
                field: 2,
            },
            upstream_ref,
        )
        .unwrap();
        let held = heap
            .alloc_array(&[table, downstream, upstream, refs[0], refs[1], refs[2]])
            .unwrap();
        let root = heap.add_root(RootOwner(0), RootKind::Host, held).unwrap();
        assert!(
            heap.model_slot(table, ModelSlot::Fields)
                .unwrap()
                .same_bits(Value::NULL)
        );
        let deleted = PropertyId {
            host: table,
            field: 1,
        };
        let claim = heap.alloc_array(&[Value::int(42)]).unwrap();
        heap.set_property_claim(deleted, claim).unwrap();
        assert_eq!(remove(&mut heap, table, 1).as_int(), Some(20));
        assert!(heap.property_claim(deleted).unwrap().same_bits(Value::NULL));
        assert_eq!(
            heap.property_ref(refs[1]).unwrap_err(),
            HeapError::RetiredHost
        );
        assert_eq!(heap.property_ref(refs[2]).unwrap().0.field, 2);
        assert_eq!(heap.model_field(table, 1).unwrap(), 2);
        collect(&mut heap);
        assert!(
            heap.info(claim).is_err(),
            "the deleted field releases its claim"
        );
        heap.table_store(upstream, 0, Value::int(0), Value::int(41))
            .unwrap();
        assert_eq!(heap.table_entry(table, 1).unwrap().1.as_int(), Some(41));
        assert_eq!(
            heap.table_entry(downstream, 2).unwrap().1.as_int(),
            Some(41)
        );
        assert_eq!(
            heap.table_entry(downstream, 1).unwrap().1.as_int(),
            Some(20)
        );
        heap.table_store(table, 2, Value::int(1), Value::int(99))
            .unwrap();
        assert_eq!(heap.model_field(table, 2).unwrap(), 3);
        assert_eq!(
            heap.property_ref(refs[1]).unwrap_err(),
            HeapError::RetiredHost
        );
        heap.table_store(table, 0, Value::int(0), Value::int(11))
            .unwrap();
        assert_eq!(
            heap.table_entry(downstream, 0).unwrap().1.as_int(),
            Some(11)
        );
        remove(&mut heap, table, 1);
        heap.table_store(upstream, 0, Value::int(0), Value::int(42))
            .unwrap();
        assert_eq!(
            heap.table_entry(downstream, 2).unwrap().1.as_int(),
            Some(41)
        );
        assert_eq!(heap.table_entry(table, 1).unwrap().1.as_int(), Some(99));
        heap.clear_collection(table).unwrap();
        heap.table_store(table, 0, Value::int(0), Value::int(100))
            .unwrap();
        assert_eq!(
            heap.property_ref(refs[0]).unwrap_err(),
            HeapError::RetiredHost
        );
        heap.release_root(root).unwrap();
        collect(&mut heap);
        assert_eq!(
            heap.stats().live_objects,
            0,
            "removed fields and reverse links do not retain the old graph"
        );
    }

    #[test]
    fn model_table_remove_mapping_and_connections_survive_snapshot_rebinding() {
        use crate::heap::snapshot as graph;
        use crate::snapshot::{SnapshotMap, SnapshotRegistry, VmSnapshot};
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let limits = crate::FrameSlotsLimits {
            initial_capacity: 8,
            max_capacity: 128,
            max_window_len: 64,
        };
        let slots = crate::FrameSlots::new(&mut heap, limits).unwrap();
        let table = model(&mut heap, &[10, 20, 30]);
        let reference = reference(&mut heap, table, 2);
        let consumer = model(&mut heap, &[0]);
        heap.bind_host_property(
            PropertyId {
                host: consumer,
                field: 0,
            },
            reference,
        )
        .unwrap();
        remove(&mut heap, table, 1);
        let held = heap.alloc_array(&[table, consumer, reference]).unwrap();
        heap.add_root(RootOwner(0), RootKind::Host, held).unwrap();
        collect(&mut heap);
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
        for _ in 0..7 {
            restored.alloc_array(&[]).unwrap();
        }
        let mut restored_slots = crate::FrameSlots::new(&mut restored, limits).unwrap();
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
        let table = map.value(table).unwrap();
        let consumer = map.value(consumer).unwrap();
        let reference = map.value(reference).unwrap();
        assert_eq!(restored.property_ref(reference).unwrap().0.field, 2);
        restored
            .table_store(table, 1, Value::int(2), Value::int(42))
            .unwrap();
        assert_eq!(
            restored.table_entry(consumer, 0).unwrap().1.as_int(),
            Some(42)
        );
        restored
            .table_store(table, 2, Value::int(1), Value::int(99))
            .unwrap();
        assert_eq!(restored.model_field(table, 2).unwrap(), 3);
    }
}
