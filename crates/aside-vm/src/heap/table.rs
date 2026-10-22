use super::*;

impl Heap {
    /// Clear a host-owned collection while retaining its identity and metadata.
    /// The detached payload is reclaimed by incremental GC. Like `array_push`,
    /// this trusted host operation may update a script-readonly collection.
    pub fn clear_collection(&mut self, target: Value) -> Result<(), HeapError> {
        let info = self.info(target)?;
        if !matches!(info.kind, ObjectKind::Array | ObjectKind::Table) {
            return Err(HeapError::WrongKind);
        }
        if info.len == 0 {
            return Ok(());
        }
        let prepared = if info.kind == ObjectKind::Table {
            self.alloc_table()?
        } else {
            self.alloc_array(&[])?
        };
        self.commit_collection(target, prepared)?;
        self.request_collection();
        Ok(())
    }

    /// Trusted-host commit of a private collection work buffer. Both objects
    /// must be rooted and have the same collection kind. Callers must not expose
    /// `prepared` to scripts; preparation crossing a mutable boundary requires
    /// checking the source content version. Kind validation precedes the swap.
    /// Target identity/metadata are retained and observers are invalidated once,
    /// without copying/freeing old edges. The old payload remains in `prepared`
    /// for incremental reclamation;
    /// callers release that root and may request collection after large trims.
    pub fn commit_collection(&mut self, target: Value, prepared: Value) -> Result<(), HeapError> {
        self.commit_collection_change(target, prepared, None)
    }

    pub(crate) fn commit_table_remove(
        &mut self,
        target: Value,
        prepared: Value,
        position: u32,
    ) -> Result<(), HeapError> {
        self.commit_collection_change(target, prepared, Some(position))
    }

    fn commit_collection_change(
        &mut self,
        target: Value,
        prepared: Value,
        removed: Option<u32>,
    ) -> Result<(), HeapError> {
        let target_value = target;
        let prepared_value = prepared;
        let target = self.key(target)?;
        let prepared = self.key(prepared)?;
        let kind = self.object(target)?.kind;
        if !matches!(kind, ObjectKind::Array | ObjectKind::Table)
            || self.object(prepared)?.kind != kind
        {
            return Err(HeapError::WrongKind);
        }
        if target == prepared {
            return Ok(());
        }
        // Removal only retains already adopted values; the prepared table's
        // metadata contains its new position map and must remain private.
        if removed.is_none() {
            self.adopt_model_contents(target_value, prepared_value)?;
        }
        if self.is_model_table(target_value)? {
            if let Some(position) = removed {
                self.disconnect_property(PropertyId {
                    host: target_value,
                    field: self.model_field(target_value, position)?,
                })?;
                self.finish_model_table_remove(target_value, prepared_value)?;
                let old = self.object(target)?.model_metadata;
                let new = self.object(prepared)?.model_metadata;
                self.barrier(old)?;
                self.barrier(new)?;
                self.objects.get_mut(target).unwrap().model_metadata = new;
                self.objects.get_mut(prepared).unwrap().model_metadata = old;
            } else {
                self.disconnect_host_properties(target_value)?;
                self.reset_model_properties(target_value)?;
            }
        }
        let empty = || Payload::Values(crate::payload::Values::default());
        let a = self.objects.get_mut(target).unwrap();
        let payload = std::mem::replace(&mut a.payload, empty());
        let charge = a.charged;
        let b = self.objects.get_mut(prepared).unwrap();
        let payload = std::mem::replace(&mut b.payload, payload);
        let charge = std::mem::replace(&mut b.charged, charge);
        let a = self.objects.get_mut(target).unwrap();
        a.payload = payload;
        a.charged = charge;
        for key in [target, prepared] {
            let object = self.objects.get_mut(key).unwrap();
            object.content_version = object.content_version.wrapping_add(1);
            object.structure_version = object.structure_version.wrapping_add(1);
            if self.gc.phase == crate::GcPhase::Mark {
                object.scan = 0;
                object.metadata_scanned = false;
                object.reactive_scanned = false;
                object.mark = self.gc.cycle;
                if !object.queued {
                    object.queued = true;
                    self.gray.push_back(key);
                }
            }
        }
        self.invalidate_reactive(target_value, None)?;
        Ok(())
    }
    pub fn table_len(&self, table: Value) -> Result<u32, HeapError> {
        let Payload::Table(values) = &self.object(self.key(table)?)?.payload else {
            return Err(HeapError::WrongKind);
        };
        Ok(values.len / 2)
    }
    pub fn alloc_table(&mut self) -> Result<Value, HeapError> {
        self.input_values(&[])?;
        self.ensure_object_slot()?;
        Ok(self.allocate(
            ObjectKind::Table,
            Payload::Table(Values::default()),
            None,
            0,
        ))
    }
    pub fn table_entry(&self, table: Value, position: u32) -> Result<(Value, Value), HeapError> {
        let Payload::Table(values) = &self.object(self.key(table)?)?.payload else {
            return Err(HeapError::WrongKind);
        };
        let index = position.checked_mul(2).ok_or(HeapError::OutOfBounds)?;
        Ok((
            values.get(index).ok_or(HeapError::OutOfBounds)?,
            self.proposed_property(table, position)
                .unwrap_or(values.get(index + 1).ok_or(HeapError::OutOfBounds)?),
        ))
    }
    /// Store at a position already resolved by the caller. Appended keys must
    /// be valid and unique under Aside key equality; overwrites retain the
    /// existing key and order. VM lookup validates keys before using this
    /// storage primitive; host adapters uphold the same table invariant.
    pub fn table_store(
        &mut self,
        table: Value,
        position: u32,
        key_value: Value,
        value: Value,
    ) -> Result<(), HeapError> {
        if self.is_model_table(table)? {
            let property = PropertyId {
                host: table,
                field: self.model_field(table, position)?,
            };
            if position < self.table_len(table)? {
                return self.write_host_property(property, value, true);
            }
            if self.is_property_ref(value) {
                let resolved = self.property_argument(value)?;
                let before = self.info(table)?;
                self.store_table_entry(table, position, key_value, resolved)?;
                if let Err(error) = self.bind_host_property(property, value) {
                    // No VM/GC step can observe the provisional field. Failed binding
                    // admission preserves the original entries and iterator versions.
                    let key = self.key(table)?;
                    let object = self.objects.get_mut(key).unwrap();
                    let Payload::Table(entries) = &mut object.payload else {
                        unreachable!()
                    };
                    let freed = entries.pop().unwrap().1 + entries.pop().unwrap().1;
                    object.charged -= freed;
                    object.observable_fields -= 1;
                    object.observable_writable -= 1;
                    object.content_version = before.content_version;
                    object.structure_version = before.structure_version;
                    self.freed(freed);
                    self.rollback_model_position(table)?;
                    return Err(error);
                }
                return Ok(());
            }
        }
        self.store_table_entry(table, position, key_value, value)
    }

    pub(super) fn store_table_entry(
        &mut self,
        table: Value,
        position: u32,
        key_value: Value,
        value: Value,
    ) -> Result<(), HeapError> {
        let key = self.key(table)?;
        let Payload::Table(values) = &self.object(key)?.payload else {
            return Err(HeapError::WrongKind);
        };
        let index = position.checked_mul(2).ok_or(HeapError::OutOfBounds)?;
        if index > values.len {
            return Err(HeapError::OutOfBounds);
        }
        let append = index == values.len;
        let extra = if append {
            Values::growth_bytes(values.len.into(), 2)
        } else {
            0
        };
        if append {
            values.len.checked_add(2).ok_or(HeapError::OutOfBounds)?;
        }
        let mapping_bytes = if append {
            self.model_append_bytes(table)?
        } else {
            0
        };
        self.ensure_bytes(extra + mapping_bytes)?;
        // Both barriers and the complete allocation preflight precede publication.
        self.adopt_model_value_reserving(table, value, extra + mapping_bytes)?;
        self.barrier(key_value)?;
        self.barrier(value)?;
        if append {
            self.append_model_position(table)?;
        }
        self.charge(extra, u64::from(extra != 0));
        let object = self.objects.get_mut(key).unwrap();
        let Payload::Table(values) = &mut object.payload else {
            unreachable!()
        };
        if append {
            values.push(key_value);
            values.push(value);
            object.structure_version = object.structure_version.wrapping_add(1);
            if object.model {
                object.observable_fields += 1;
                object.observable_writable += 1;
            }
        } else {
            values.set(index + 1, value);
        }
        object.content_version = object.content_version.wrapping_add(1);
        object.charged += extra;
        self.invalidate_reactive(table, Some(self.model_field(table, position)?))?;
        Ok(())
    }

    /// Replace an existing property slot after complete graph preparation.
    /// Adoption and capacity checks precede publication in prepare_properties.
    pub(super) fn store_prepared_property_value(
        &mut self,
        table: Value,
        position: u32,
        value: Value,
    ) -> Result<(), HeapError> {
        let key = self.key(table)?;
        let field = self.model_field(table, position)?;
        self.barrier(value)?;
        let object = self.objects.get_mut(key).unwrap();
        let Payload::Table(values) = &mut object.payload else {
            unreachable!()
        };
        values.set(position * 2 + 1, value);
        object.content_version = object.content_version.wrapping_add(1);
        self.invalidate_reactive(table, Some(field))
    }
}

/// A borrowed-key, allocation-free own-slot lookup. This is not a GC root:
/// retain the table through a host root or the native ticket for its lifetime.
/// It never follows delegates or invokes hooks. Drop the cursor to cancel it.
#[derive(Debug)]
pub struct TableStringLookup<'a> {
    table: Value,
    key: &'a str,
    version: u64,
    entry: u32,
    offset: Option<u32>,
    result: Option<Option<Value>>,
}
#[derive(Clone, Copy, Debug)]
pub enum TableLookupStatus {
    Pending,
    Complete(Option<Value>),
}
#[derive(Clone, Copy, Debug)]
pub struct TableLookupProgress {
    pub work: u32,
    pub status: TableLookupStatus,
}
impl Heap {
    /// Length of an array, tuple or private dependency sequence, without traversal.
    pub fn sequence_len(&self, sequence: Value) -> Result<u32, HeapError> {
        let info = self.info(sequence)?;
        if !matches!(
            info.kind,
            ObjectKind::Array | ObjectKind::Tuple | ObjectKind::Dependencies
        ) {
            return Err(HeapError::WrongKind);
        }
        Ok(info.len)
    }
    /// One semantic sequence read; the caller owns the budget and retention.
    pub fn sequence_get(&self, sequence: Value, index: u32) -> Result<Value, HeapError> {
        self.sequence_len(sequence)?;
        self.get_field(sequence, index)
    }
    pub fn table_own_string_lookup<'a>(
        &self,
        table: Value,
        key: &'a str,
    ) -> Result<TableStringLookup<'a>, HeapError> {
        let object = self.object(self.key(table)?)?;
        if !matches!(object.payload, Payload::Table(_)) {
            return Err(HeapError::WrongKind);
        }
        Ok(TableStringLookup {
            table,
            key,
            version: object.content_version,
            entry: 0,
            offset: None,
            result: None,
        })
    }
    /// Each examined entry, compared byte, and completion costs one work unit.
    /// No allocation, hidden linear scan, or whole-string comparison occurs.
    /// Any content mutation invalidates the cursor, including value overwrites.
    /// Complete(Some(NULL)) means a present null, distinct from Complete(None).
    pub fn table_own_string_step(
        &self,
        cursor: &mut TableStringLookup<'_>,
        budget: u32,
    ) -> Result<TableLookupProgress, HeapError> {
        let info = self.info(cursor.table)?;
        if info.content_version != cursor.version {
            return Err(HeapError::ConcurrentModification);
        }
        let mut work = 0;
        while work < budget && cursor.result.is_none() {
            work += 1;
            if cursor.entry == info.len {
                cursor.result = Some(None);
                break;
            }
            let (key, value) = self.table_entry(cursor.table, cursor.entry)?;
            if let Some(offset) = cursor.offset {
                if offset as usize == cursor.key.len() {
                    cursor.result = Some(Some(value));
                    break;
                }
                let mut byte = [0];
                self.read_bytes(key, offset, &mut byte)?;
                if byte[0] == cursor.key.as_bytes()[offset as usize] {
                    cursor.offset = Some(offset + 1);
                } else {
                    cursor.entry += 1;
                    cursor.offset = None;
                }
            } else if key.tag() == crate::ValueTag::Object {
                let key_info = self.info(key)?;
                if key_info.kind == ObjectKind::String && key_info.len as usize == cursor.key.len()
                {
                    cursor.offset = Some(0);
                } else {
                    cursor.entry += 1;
                }
            } else {
                cursor.entry += 1;
            }
        }
        Ok(TableLookupProgress {
            work,
            status: match cursor.result {
                Some(result) => TableLookupStatus::Complete(result),
                None => TableLookupStatus::Pending,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committing_into_a_black_object_requeues_moved_white_edges_without_allocation() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let target = heap.alloc_array(&[]).unwrap();
        let child = heap.alloc_array(&[Value::int(42)]).unwrap();
        let prepared = heap.alloc_array(&[child]).unwrap();
        heap.add_root(RootOwner(1), RootKind::Host, target).unwrap();
        heap.add_root(RootOwner(1), RootKind::Host, prepared)
            .unwrap();
        heap.begin_collection().unwrap();
        let key = heap.key(target).unwrap();
        loop {
            heap.gc_step(crate::GcBudget {
                units: 1,
                ..Default::default()
            })
            .unwrap();
            let object = heap.objects.get(key).unwrap();
            if object.mark == heap.gc.cycle && !object.queued {
                break;
            }
        }
        assert_ne!(
            heap.objects.get(heap.key(child).unwrap()).unwrap().mark,
            heap.gc.cycle
        );
        let charge = heap.stats().charged_bytes;
        let allocations =
            allocation_counter::measure(|| heap.commit_collection(target, prepared).unwrap());
        assert_eq!(allocations.count_total, 0);
        assert_eq!(heap.stats().charged_bytes, charge);
        while heap.phase() != crate::GcPhase::Idle {
            heap.gc_step(crate::GcBudget::default()).unwrap();
        }
        assert!(heap.sequence_get(target, 0).unwrap().same_bits(child));
        assert_eq!(heap.sequence_get(child, 0).unwrap().as_int(), Some(42));
        assert_eq!(heap.sequence_len(prepared).unwrap(), 0);
    }
}
