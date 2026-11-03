//! Standard-library indexing for short immutable text keys. Ordered values
//! remain the source of truth; snapshots rebuild this index as lookups proceed.
use super::*;
use crate::payload::table::{INDEX_BASE_BYTES, INDEX_ENTRY_BYTES, KEY_BYTES, TextIndex};

impl Heap {
    pub(crate) fn text_key_hash(&self, key: Value) -> Result<Option<u64>, HeapError> {
        if key.tag() != crate::ValueTag::Object {
            return Ok(None);
        }
        let object = self.object(self.key(key)?)?;
        if !matches!(object.kind, ObjectKind::String | ObjectKind::Bytes) {
            return Ok(None);
        }
        let Payload::Bytes(bytes) = &object.payload else {
            unreachable!()
        };
        if bytes.len as usize > KEY_BYTES {
            return Ok(None);
        }
        Ok(Some(crate::payload::table::text_hash(
            object.kind == ObjectKind::String,
            bytes.segment(0),
        )))
    }

    /// Worst-case admission for adding one key. Each probe builds at most one
    /// index entry, so neither first use nor snapshot restore walks a whole table.
    pub(crate) fn table_index_bytes(&self, table: Value) -> Result<u64, HeapError> {
        let Payload::Table(table) = &self.object(self.key(table)?)?.payload else {
            return Err(HeapError::WrongKind);
        };
        Ok(match &table.index {
            None => INDEX_BASE_BYTES + INDEX_ENTRY_BYTES,
            Some(index) if index.scanned < table.values.len / 2 => INDEX_ENTRY_BYTES,
            _ => 0,
        })
    }

    /// Return the next hash candidate or table length. Hash collisions still use
    /// Aside equality in the caller, one candidate at a time.
    pub(crate) fn table_index_candidate(
        &mut self,
        table_value: Value,
        hash: u64,
        from: u32,
    ) -> Result<u32, HeapError> {
        let object_key = self.key(table_value)?;
        let Payload::Table(table) = &self.object(object_key)?.payload else {
            return Err(HeapError::WrongKind);
        };
        let len = table.values.len / 2;
        let scanned = table.index.as_ref().map_or(0, |index| index.scanned);
        if let Some(index) = &table.index {
            let candidate = index.candidate(hash, from);
            if candidate < scanned || scanned == len {
                return Ok(candidate.min(len));
            }
        }
        // Build from the first unindexed key even if a small-budget scan has
        // already passed it. Keep advancing the caller from its own position.
        let key = table.values.get(scanned * 2).unwrap();
        let fingerprint = self.text_key_hash(key)?;
        let extra = u64::from(table.index.is_none()) * INDEX_BASE_BYTES
            + u64::from(fingerprint.is_some()) * INDEX_ENTRY_BYTES;
        self.ensure_bytes(extra)?;
        self.charge(extra, 0);
        let object = self.objects.get_mut(object_key).unwrap();
        let Payload::Table(table) = &mut object.payload else {
            unreachable!()
        };
        let index = table.index.get_or_insert_with(Box::<TextIndex>::default);
        if let Some(fingerprint) = fingerprint {
            index.entries.insert((fingerprint, scanned));
            index.reserved += INDEX_ENTRY_BYTES;
        }
        index.scanned += 1;
        object.charged += extra;
        // The existing prefix had no candidate. Only the newly inspected key
        // can match; a second tree search would repeat the same prefix lookup.
        Ok(if fingerprint == Some(hash) && scanned >= from {
            scanned
        } else {
            index.scanned.max(from).min(len)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GcBudget, GcPhase, HeapLimits};

    fn populated() -> (Heap, Value) {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let table = heap.alloc_table().unwrap();
        for position in 0..256 {
            let key = heap.alloc_string(&format!("field-{position}")).unwrap();
            heap.table_store(table, position, key, Value::int(i64::from(position)))
                .unwrap();
        }
        let missing = crate::payload::table::text_hash(true, b"missing");
        let mut position = 0;
        while position < 256 {
            position = heap
                .table_index_candidate(table, missing, position)
                .unwrap();
        }
        (heap, table)
    }

    fn lookup(heap: &Heap, table: Value, key: &str, budget: u32) -> (Option<Value>, u32) {
        let mut cursor = heap.table_own_string_lookup(table, key).unwrap();
        let mut work = 0;
        loop {
            let progress = heap.table_own_string_step(&mut cursor, budget).unwrap();
            assert!(progress.work <= budget);
            work += progress.work;
            if let TableLookupStatus::Complete(value) = progress.status {
                return (value, work);
            }
        }
    }

    #[test]
    fn host_reads_share_the_index_and_keep_small_budget_and_append_semantics() {
        let (mut heap, table) = populated();
        for budget in [1, 7, 4096] {
            assert_eq!(
                lookup(&heap, table, "field-255", budget)
                    .0
                    .unwrap()
                    .as_int(),
                Some(255)
            );
            assert!(lookup(&heap, table, "missing", budget).0.is_none());
        }
        assert!(
            lookup(&heap, table, "field-255", 4096).1 * 2 < lookup(&heap, table, "field-255", 1).1
        );
        let key = heap.table_key(table, 255).unwrap();
        heap.table_store(table, 255, key, Value::NULL).unwrap();
        assert!(
            lookup(&heap, table, "field-255", 4096)
                .0
                .unwrap()
                .same_bits(Value::NULL)
        );
        let key = heap.alloc_string("appended").unwrap();
        heap.table_store(table, 256, key, Value::int(99)).unwrap();
        assert_eq!(
            lookup(&heap, table, "appended", 4096).0.unwrap().as_int(),
            Some(99)
        );
        heap.clear_collection(table).unwrap();
        heap.table_store(table, 0, key, Value::int(100)).unwrap();
        assert_eq!(
            lookup(&heap, table, "appended", 4096).0.unwrap().as_int(),
            Some(100)
        );
    }

    #[test]
    fn incremental_collection_refunds_the_complete_index() {
        let (mut heap, _) = populated();
        heap.begin_collection().unwrap();
        let mut steps = 0;
        while heap.phase() != GcPhase::Idle {
            let progress = heap
                .gc_step(GcBudget {
                    units: 1,
                    ..GcBudget::default()
                })
                .unwrap();
            assert!(progress.units <= 1);
            steps += 1;
            assert!(steps < 20_000);
        }
        assert_eq!(heap.stats().live_objects, 0);
        assert_eq!(heap.stats().payload_bytes, 0);
    }
}
