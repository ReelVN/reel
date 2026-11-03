//! Budgeted access to the table-owned standard-library index.
use super::*;
use crate::payload::table::{KEY_BYTES, MIN_INDEX_LEN};

impl Vm {
    pub(super) fn indexed_table_candidate(
        &mut self,
        frame: &ScriptFrame,
        table: Value,
        len: u32,
        meter: &mut Meter,
    ) -> Result<Option<u32>, VmError> {
        #[cfg(test)]
        if self.work_probe.linear_table_scan {
            return Ok(None);
        }
        if len < MIN_INDEX_LEN {
            return Ok(None);
        }
        let key = self.value(frame.env, 2)?;
        if key.tag() != ValueTag::Object {
            return Ok(None);
        }
        let info = self.heap.info(key)?;
        if !matches!(info.kind, ObjectKind::String | ObjectKind::Bytes)
            || info.len as usize > KEY_BYTES
        {
            return Ok(None);
        }
        let allocation = self.heap.table_index_bytes(table)?;
        // Tree node searches and an optional insertion are logarithmic. Bound
        // both text reads before hashing; collision equality is charged normally.
        let work = 1
            + 2 * u64::from(info.len)
            + crate::payload::table::search_work(len)
            + if allocation != 0 {
                2 * KEY_BYTES as u64
            } else {
                0
            };
        // Index storage is optional. A tight slice or full heap continues the
        // existing resumable scan without introducing allocation failures in reads.
        if !meter.admits(0, work, allocation) || allocation > self.heap.usage.remaining_bytes() {
            return Ok(None);
        }
        assert!(meter.take(0, work, allocation));
        let hash = self.heap.text_key_hash(key)?.unwrap();
        let from = frame.table_work.unwrap().entry;
        Ok(Some(self.heap.table_index_candidate(table, hash, from)?))
    }
}
