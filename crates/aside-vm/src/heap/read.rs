//! Short immutable object borrows retain the ordinary handle and field checks.
use super::*;

/// One validated object for synchronous read-only work. The heap borrow excludes
/// mutation, collection and object-slot reuse until the view's final use.
#[derive(Clone, Copy)]
pub(crate) struct ObjectRead<'a> {
    object: &'a Object,
}

impl Heap {
    /// Resolve heap identity, full generation, retirement and GC cutoff once.
    #[inline]
    pub(crate) fn read_object(&self, value: Value) -> Result<ObjectRead<'_>, HeapError> {
        Ok(ObjectRead {
            object: self.object(self.key(value)?)?,
        })
    }
}

impl ObjectRead<'_> {
    #[inline]
    pub(crate) fn kind(self) -> ObjectKind {
        self.object.kind
    }

    #[inline]
    pub(crate) fn info(self) -> ObjectInfo {
        ObjectInfo {
            kind: self.object.kind,
            len: match &self.object.payload {
                Payload::Table(values) => values.len / 2,
                _ => self.object.payload.len(),
            },
            content_version: self.object.content_version,
            structure_version: self.object.structure_version,
        }
    }

    /// Keep kind, logical bounds and Cell initialization checks for every field.
    #[inline]
    pub(crate) fn field(self, position: u32) -> Result<Value, HeapError> {
        let obj = self.object;
        if !obj.kind.values() || matches!(obj.payload, Payload::Table(_)) {
            return Err(HeapError::WrongKind);
        }
        if let Payload::Cell(cell) = &obj.payload {
            if position != 0 {
                return Err(HeapError::OutOfBounds);
            }
            return cell.value.ok_or(HeapError::UninitializedCell);
        }
        obj.payload.edge(position).ok_or(HeapError::OutOfBounds)
    }
}

#[cfg(test)]
mod tests;
