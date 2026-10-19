//! Synchronous scans of the existing dependency blocks, without a graph copy.
use super::*;

pub(super) struct DependencyCursor {
    dependencies: Value,
    pub offset: u32,
    values: [Value; crate::payload::VALUE_CHUNK_LEN],
    within: usize,
    len: usize,
}

impl DependencyCursor {
    pub fn new(dependencies: Value) -> Self {
        Self {
            dependencies,
            offset: 0,
            values: [Value::NULL; crate::payload::VALUE_CHUNK_LEN],
            within: 0,
            len: 0,
        }
    }

    /// The candidate or committed dependency payload owns every copied edge.
    /// This cursor stays within one synchronous operation: no script, yield or
    /// GC step runs between reads. Copying the block releases the heap borrow
    /// so normal subscription admission may allocate or update other objects.
    pub fn next(&mut self, heap: &Heap) -> Result<Option<(Value, Value)>, HeapError> {
        if self.within == self.len {
            let object = heap.object(heap.key(self.dependencies)?)?;
            let Payload::Dependencies(dependencies) = &object.payload else {
                return Err(HeapError::WrongKind);
            };
            let segment = dependencies.values.segment(self.offset);
            self.len = segment.len();
            self.within = 0;
            if self.len == 0 {
                return Ok(None);
            }
            self.values[..self.len].copy_from_slice(segment);
        }
        let pair = (self.values[self.within], self.values[self.within + 1]);
        self.within += 2;
        self.offset += 2;
        Ok(Some(pair))
    }
}
