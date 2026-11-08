//! Restore allocation preserves the order used by existing identity-key indices.
use crate::{
    Heap, HeapError,
    root_ranges::Reservation,
    snapshot::{Record, RecordKind, SnapshotError},
};
use std::{cmp::Reverse, collections::BinaryHeap};

#[derive(Default)]
pub(super) struct ObjectOrder {
    // Release the allocation before refunding its reservation.
    entries: BinaryHeap<Reverse<(u64, usize)>>,
    charge: Option<Reservation>,
    cursor: usize,
    count: usize,
    phase: u8,
}

#[cfg(test)]
mod tests;
pub(super) enum Step {
    Work,
    Allocate(usize),
    Done,
}
impl ObjectOrder {
    /// Each unit scans one record, inserts one identity or allocates one object.
    /// Standard-library heap operations have logarithmic cost in object count.
    pub(super) fn step(&mut self, heap: &Heap, records: &[Record]) -> Result<Step, SnapshotError> {
        match self.phase {
            0 => {
                if let Some(record) = records.get(self.cursor) {
                    self.count += usize::from(record.kind == RecordKind::Object);
                    self.cursor += 1;
                } else {
                    let bytes = self
                        .count
                        .checked_mul(size_of::<Reverse<(u64, usize)>>())
                        .filter(|&n| n <= isize::MAX as usize)
                        .ok_or(HeapError::InvalidLimits)?;
                    let mut charge = heap.budget_usage().reserve(bytes as u64)?;
                    let mut entries = BinaryHeap::new();
                    entries
                        .try_reserve_exact(self.count)
                        .map_err(|_| HeapError::AllocationFailure)?;
                    let actual = entries
                        .capacity()
                        .checked_mul(size_of::<Reverse<(u64, usize)>>())
                        .ok_or(HeapError::InvalidLimits)?;
                    charge.resize(actual as u64)?;
                    self.entries = entries;
                    self.charge = Some(charge);
                    self.cursor = 0;
                    self.phase = 1;
                }
                Ok(Step::Work)
            }
            1 => {
                if let Some(record) = records.get(self.cursor) {
                    if record.kind == RecordKind::Object {
                        // HeapHandle.key uses this u64 order. Slotmap's KeyData
                        // Ord instead puts slot index before generation.
                        self.entries.push(Reverse((record.key, self.cursor)));
                    }
                    self.cursor += 1;
                } else {
                    self.phase = 2;
                }
                Ok(Step::Work)
            }
            _ => {
                if let Some(Reverse((_, record))) = self.entries.pop() {
                    Ok(Step::Allocate(record))
                } else {
                    self.entries = BinaryHeap::new();
                    self.charge = None;
                    Ok(Step::Done)
                }
            }
        }
    }
}
