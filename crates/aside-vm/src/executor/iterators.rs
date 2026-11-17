//! Constant-space range and sequence cursors share the ordinary iterator protocol.
use super::*;
use crate::heap::ObjectRead;

pub(super) struct Iteration {
    pub done: bool,
    pub value: Value,
    next: Option<i64>,
    chain_next: Option<Value>,
}

impl Vm {
    pub(super) fn iterator_new(&mut self, source: Value) -> Result<Value, HeapError> {
        self.heap.observe_read(source, None)?;
        let info = self.heap.info(source)?;
        if info.kind == ObjectKind::Generator {
            return Ok(source);
        }
        let cursor = if matches!(info.kind, ObjectKind::FaultStack | ObjectKind::FaultCause) {
            self.heap.fault_sequence_head(source)?
        } else if info.kind == ObjectKind::Range {
            self.heap.get_field(source, 0)?
        } else {
            self.heap.sequence_len(source)?;
            Value::int(0)
        };
        self.heap.alloc_record(
            ObjectKind::Iterator,
            &[source, cursor, Value::int(info.structure_version as i64)],
        )
    }

    pub(super) fn iterator_peek(&self, iterator: ObjectRead<'_>) -> Result<Iteration, HeapError> {
        if iterator.kind() != ObjectKind::Iterator {
            return Err(HeapError::WrongKind);
        }
        let source = iterator.field(0)?;
        let source = if source.tag() == ValueTag::Null {
            None
        } else {
            Some(self.heap.read_object(source)?)
        };
        if source.is_some_and(|source| {
            matches!(
                source.kind(),
                ObjectKind::FaultStack | ObjectKind::FaultCause
            )
        }) {
            let current = iterator.field(1)?;
            let done = current.tag() == ValueTag::Null;
            return Ok(Iteration {
                done,
                value: current,
                next: None,
                chain_next: Some(if done {
                    Value::NULL
                } else {
                    self.heap.fault_sequence_next(current)?
                }),
            });
        }
        let cursor = iterator.field(1)?.as_int().ok_or(HeapError::WrongKind)?;
        let (done, value, next) = match source {
            None => (true, Value::NULL, None),
            Some(source) if source.kind() == ObjectKind::Range => {
                let end = source.field(1)?.as_int().ok_or(HeapError::WrongKind)?;
                let inclusive = source.field(2)?.as_bool().ok_or(HeapError::WrongKind)?;
                let done = cursor > end || (!inclusive && cursor == end);
                (
                    done,
                    if done {
                        Value::NULL
                    } else {
                        Value::int(cursor)
                    },
                    cursor.checked_add(1),
                )
            }
            Some(source) => {
                let info = source.info();
                let version = iterator.field(2)?.as_int().ok_or(HeapError::WrongKind)? as u64;
                if info.structure_version != version {
                    return Err(HeapError::ConcurrentModification);
                }
                let index = u32::try_from(cursor).map_err(|_| HeapError::OutOfBounds)?;
                let done = index == info.len;
                (
                    done,
                    if done {
                        Value::NULL
                    } else {
                        source.sequence_get(index)?
                    },
                    Some(cursor + 1),
                )
            }
        };
        Ok(Iteration {
            done,
            value,
            next,
            chain_next: None,
        })
    }
    pub(super) fn iterator_commit(
        &mut self,
        iterator: Value,
        step: Iteration,
    ) -> Result<(), HeapError> {
        if let Some(next) = step.chain_next {
            self.heap.set_field(iterator, 1, next)?;
            return Ok(());
        }
        match (step.done, step.next) {
            (false, Some(next)) => self.heap.set_field(iterator, 1, Value::int(next))?,
            _ => self.heap.set_field(iterator, 0, Value::NULL)?,
        }
        Ok(())
    }
}
