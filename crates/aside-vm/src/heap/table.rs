use super::*;

impl Heap {
    pub fn table_len(&self, table: Value) -> Result<u32, HeapError> {
        let Payload::Table(values) = &self.object(self.key(table)?)?.payload else {
            return Err(HeapError::WrongKind);
        };
        Ok(values.len / 2)
    }
    pub fn alloc_table(&mut self) -> Result<Value, HeapError> {
        self.input_values(&[])?;
        if self.objects.full() {
            return Err(HeapError::ObjectCapacity);
        }
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
            values.get(index + 1).ok_or(HeapError::OutOfBounds)?,
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
        let key = self.key(table)?;
        let Payload::Table(values) = &self.object(key)?.payload else {
            return Err(HeapError::WrongKind);
        };
        let index = position.checked_mul(2).ok_or(HeapError::OutOfBounds)?;
        if index > values.len {
            return Err(HeapError::OutOfBounds);
        }
        let append = index == values.len;
        let extra = if append && values.needs_chunk() {
            VALUE_CHUNK_BYTES
        } else {
            0
        };
        if append {
            values.len.checked_add(2).ok_or(HeapError::OutOfBounds)?;
        }
        self.ensure_bytes(extra)?;
        // Both barriers and the complete allocation preflight precede publication.
        self.barrier(key_value)?;
        self.barrier(value)?;
        self.charge(extra, u64::from(extra != 0));
        let object = self.objects.get_mut(key).unwrap();
        let Payload::Table(values) = &mut object.payload else {
            unreachable!()
        };
        if append {
            values.push(key_value);
            values.push(value);
            object.structure_version = object.structure_version.wrapping_add(1);
        } else {
            values.set(index + 1, value);
        }
        object.content_version = object.content_version.wrapping_add(1);
        object.charged += extra;
        Ok(())
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
    /// Length of an array or tuple, with no traversal or implicit conversion.
    pub fn sequence_len(&self, sequence: Value) -> Result<u32, HeapError> {
        let info = self.info(sequence)?;
        if !matches!(info.kind, ObjectKind::Array | ObjectKind::Tuple) {
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
