//! Bounded, non-owning mutation history for host projections.
use super::{Heap, HeapError, HeapHandle, HeapId, HeapKey, SessionEpoch, Value};
use slotmap::Key;

const CAPACITY: usize = 512;

/// A position in one heap session's watched-object history. Host projections
/// rebuild their dependencies after restoration or history overflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChangeCursor {
    heap: HeapId,
    epoch: SessionEpoch,
    revision: u64,
}

struct Log {
    keys: [u64; CAPACITY],
    revision: u64,
    _charge: crate::HeapReservation,
}
#[derive(Default)]
pub(crate) struct Changes(Option<Box<Log>>);
impl Changes {
    pub(crate) fn record(&mut self, key: HeapKey, watched: bool) {
        if watched {
            let log = self
                .0
                .as_mut()
                .expect("watch admission precedes publication");
            log.keys[log.revision as usize % CAPACITY] = key.data().as_ffi();
            log.revision = log.revision.wrapping_add(1);
        }
    }
}
impl Heap {
    /// Observe content writes and retirement of an object without retaining it.
    /// The shared 512-entry history is charged to the heap on first use. Repeated
    /// observation is allocation-free; observation ends when the object retires.
    pub fn watch_object(&mut self, value: Value) -> Result<HeapHandle, HeapError> {
        let key = self.key(value)?;
        self.object(key)?;
        if self.changes.0.is_none() {
            let charge = self.budget_usage().reserve(size_of::<Log>() as u64)?;
            self.changes.0 = Some(Box::new(Log {
                keys: [0; CAPACITY],
                revision: 0,
                _charge: charge,
            }));
        }
        self.objects.get_mut(key).unwrap().watched = true;
        self.handle(value)
    }

    /// Capture a position after publishing a host projection's dependencies.
    pub fn change_cursor(&self) -> ChangeCursor {
        ChangeCursor {
            heap: self.id,
            epoch: self.epoch,
            revision: self.changes.0.as_ref().map_or(0, |log| log.revision),
        }
    }

    /// Return changed weak identities, including repeats and retired objects.
    /// `None` requires a full projection: the cursor belongs to another session
    /// or more writes occurred than the bounded history retains.
    pub fn changes_since(
        &self,
        cursor: ChangeCursor,
    ) -> Option<impl Iterator<Item = HeapHandle> + '_> {
        let current = self.change_cursor();
        if cursor.heap != current.heap || cursor.epoch != current.epoch {
            return None;
        }
        let count = current.revision.checked_sub(cursor.revision)?;
        if count > CAPACITY as u64 {
            return None;
        }
        Some((0..count).map(move |offset| {
            let log = self.changes.0.as_ref().expect("nonempty watched history");
            HeapHandle {
                heap: self.id,
                epoch: self.epoch,
                key: log.keys[(cursor.revision.wrapping_add(offset) as usize) % CAPACITY],
                _thread: Default::default(),
            }
        }))
    }
}

#[cfg(test)]
mod tests;
