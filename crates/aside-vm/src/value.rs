use std::sync::atomic::{AtomicU32, Ordering};

/// Process-local, never reused. A restored session allocates a new heap identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct HeapId(pub(crate) u32);

// Allocate ids 1..u32::MAX-1, then fail permanently before any wrap/reuse.
static NEXT_HEAP: AtomicU32 = AtomicU32::new(1);

fn fresh_id(next: &AtomicU32) -> Option<HeapId> {
    next.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .ok()
        .map(HeapId)
}

impl HeapId {
    pub(crate) fn fresh() -> Option<Self> {
        fresh_id(&NEXT_HEAP)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct SessionEpoch(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ValueTag {
    Null,
    Bool,
    Int,
    Float,
    Object,
}

/// Internal runtime representation, not a serialized or C ABI encoding.
/// Copying this value neither retains an object nor changes a reference count.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Value {
    payload: u64,
    heap: HeapId,
    tag: ValueTag,
    // Prevent crossing threads, including into another thread-local HeapId domain.
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl Value {
    pub const NULL: Self = Self::scalar(ValueTag::Null, 0);
    const fn scalar(tag: ValueTag, payload: u64) -> Self {
        Self {
            payload,
            heap: HeapId(0),
            tag,
            _thread: std::marker::PhantomData,
        }
    }
    pub const fn bool(value: bool) -> Self {
        Self::scalar(ValueTag::Bool, value as u64)
    }
    pub const fn int(value: i64) -> Self {
        Self::scalar(ValueTag::Int, value as u64)
    }
    pub const fn float(value: f64) -> Self {
        Self::scalar(ValueTag::Float, value.to_bits())
    }
    pub const fn tag(self) -> ValueTag {
        self.tag
    }
    pub const fn as_bool(self) -> Option<bool> {
        if matches!(self.tag, ValueTag::Bool) {
            Some(self.payload != 0)
        } else {
            None
        }
    }
    pub const fn as_int(self) -> Option<i64> {
        if matches!(self.tag, ValueTag::Int) {
            Some(self.payload as i64)
        } else {
            None
        }
    }
    pub const fn as_float(self) -> Option<f64> {
        if matches!(self.tag, ValueTag::Float) {
            Some(f64::from_bits(self.payload))
        } else {
            None
        }
    }
    /// Storage comparison only; language equality requires object/numeric semantics.
    pub const fn same_bits(self, other: Self) -> bool {
        self.tag as u32 == other.tag as u32
            && self.heap.0 == other.heap.0
            && self.payload == other.payload
    }
    pub(crate) const fn object(heap: HeapId, key: u64) -> Self {
        Self {
            payload: key,
            heap,
            tag: ValueTag::Object,
            _thread: std::marker::PhantomData,
        }
    }
    pub(crate) const fn object_parts(self) -> Option<(HeapId, u64)> {
        if matches!(self.tag, ValueTag::Object) {
            Some((self.heap, self.payload))
        } else {
            None
        }
    }
}

/// Host-side scoped handle. Long-term retention still requires a RootToken.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct HeapHandle {
    pub heap: HeapId,
    pub epoch: SessionEpoch,
    pub key: u64,
    pub(crate) _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct WeakHandle(pub(crate) HeapHandle);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn heap_identity_exhaustion_never_wraps() {
        let counter = AtomicU32::new(u32::MAX - 1);
        assert_eq!(fresh_id(&counter).unwrap().get(), u32::MAX - 1);
        assert!(fresh_id(&counter).is_none());
        assert!(fresh_id(&counter).is_none());
        assert_eq!(counter.load(Ordering::Relaxed), u32::MAX);
    }
}
