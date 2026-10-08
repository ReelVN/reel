//! Design experiment only: not a VM API, wire format, or collector.
use std::mem::size_of;

pub mod trace;

pub const FIELD_MAX: u32 = (1 << 24) - 1;
pub const SMALL_MIN: i64 = -(1 << 47);
pub const SMALL_MAX: i64 = (1 << 47) - 1;
const PAYLOAD_MASK: u64 = (1 << 48) - 1;
const CANONICAL_NAN: u64 = 0x7ff8_0000_0000_0000;
const NO_SLOT: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle(u64);

impl Handle {
    pub fn new(index: u32, generation: u32) -> Option<Self> {
        (index <= FIELD_MAX && (1..=FIELD_MAX).contains(&generation))
            .then_some(Self(index as u64 | ((generation as u64) << 24)))
    }
    pub fn index(self) -> u32 {
        (self.0 & FIELD_MAX as u64) as u32
    }
    pub fn generation(self) -> u32 {
        (self.0 >> 24) as u32
    }
    pub fn bits(self) -> u64 {
        self.0
    }
}

#[repr(C)]
struct Slot<T> {
    generation: u32,
    next_free: u32,
    value: Option<Box<T>>,
}

/// Both representations use this exact bounds/generation check and Box lookup.
/// A generation-0xffffff slot is permanently retired on release, never wrapped.
pub struct Arena<T> {
    slots: Vec<Slot<T>>,
    free: u32,
    live: usize,
    box_allocations: usize,
    table_allocations: usize,
    table_reallocations: usize,
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            free: NO_SLOT,
            live: 0,
            box_allocations: 0,
            table_allocations: 0,
            table_reallocations: 0,
        }
    }
}

impl<T> Arena<T> {
    pub fn insert(&mut self, value: T) -> Handle {
        let index;
        if self.free != NO_SLOT {
            index = self.free;
            let slot = &mut self.slots[index as usize];
            self.free = slot.next_free;
            slot.next_free = NO_SLOT;
            slot.value = Some(Box::new(value));
        } else {
            assert!(
                self.slots.len() <= FIELD_MAX as usize,
                "24-bit index exhausted"
            );
            index = self.slots.len() as u32;
            let old_capacity = self.slots.capacity();
            self.slots.push(Slot {
                generation: 1,
                next_free: NO_SLOT,
                value: Some(Box::new(value)),
            });
            if self.slots.capacity() != old_capacity {
                if old_capacity == 0 {
                    self.table_allocations += 1;
                } else {
                    self.table_reallocations += 1;
                }
            }
        }
        self.live += 1;
        self.box_allocations += 1;
        Handle::new(index, self.slots[index as usize].generation).unwrap()
    }
    pub fn get(&self, handle: Handle) -> Option<&T> {
        let slot = self.slots.get(handle.index() as usize)?;
        if slot.generation != handle.generation() {
            return None;
        }
        slot.value.as_deref()
    }
    pub fn get_mut(&mut self, handle: Handle) -> Option<&mut T> {
        let slot = self.slots.get_mut(handle.index() as usize)?;
        if slot.generation != handle.generation() {
            return None;
        }
        slot.value.as_deref_mut()
    }
    pub fn remove(&mut self, handle: Handle) -> bool {
        let Some(slot) = self.slots.get_mut(handle.index() as usize) else {
            return false;
        };
        if slot.generation != handle.generation() || slot.value.is_none() {
            return false;
        }
        // Drop the individual Box now; the slot table remains until Arena drop.
        slot.value = None;
        self.live -= 1;
        if slot.generation < FIELD_MAX {
            slot.generation += 1;
            slot.next_free = self.free;
            self.free = handle.index();
        }
        true
    }
    pub fn storage(&self) -> ArenaStorage {
        ArenaStorage {
            live: self.live,
            table_bytes: self.slots.capacity() * size_of::<Slot<T>>(),
            box_bytes: self.live * size_of::<T>(),
            box_allocations: self.box_allocations,
            table_allocations: self.table_allocations,
            table_reallocations: self.table_reallocations,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArenaStorage {
    pub live: usize,
    pub table_bytes: usize,
    pub box_bytes: usize,
    pub box_allocations: usize,
    pub table_allocations: usize,
    pub table_reallocations: usize,
}

#[repr(C)]
pub struct Object {
    pub payload: u64,
}

#[derive(Default)]
pub struct Heap {
    pub objects: Arena<Object>,
    pub wide: Arena<i64>,
}

#[derive(Clone, Copy, Debug)]
pub enum Input {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Object(u64),
}

pub fn normalized_float_bits(value: f64) -> u64 {
    if value.is_nan() {
        CANONICAL_NAN
    } else {
        value.to_bits()
    }
}

// Explicit 64-bit tag and payload; alignment also fixed on 32-bit targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
enum Tag {
    Null,
    Bool,
    Int,
    Float,
    Object,
}

#[derive(Clone, Copy)]
#[repr(C, align(8))]
pub struct Explicit {
    tag: Tag,
    payload: u64,
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct NanBox(u64);

// Positive quiet NaNs with nonzero upper payload form the five reserved tags.
// All input NaNs become 0x7ff8_0000_0000_0000, outside this tag range.
const NULL: u64 = 0x7ff9_0000_0000_0000;
const BOOL: u64 = 0x7ffa_0000_0000_0000;
const INT: u64 = 0x7ffb_0000_0000_0000;
const OBJECT: u64 = 0x7ffc_0000_0000_0000;
const WIDE: u64 = 0x7ffd_0000_0000_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Null,
    Bool,
    Int,
    Float,
    Object,
}

/// Copy here is for benchmark reads only. Batch owns each allocation exactly
/// once; arbitrary copied owning Values would require a VM ownership policy.
pub trait Representation: Copy {
    const NAME: &'static str;
    fn encode(input: Input, heap: &mut Heap) -> Self;
    fn kind(self) -> Kind;
    fn boolean(self) -> bool;
    fn integer(self, heap: &Heap) -> i64;
    fn float(self) -> f64;
    fn object(self) -> Handle;
    fn release(self, heap: &mut Heap);
}

impl Representation for Explicit {
    const NAME: &'static str = "explicit16";
    fn encode(input: Input, heap: &mut Heap) -> Self {
        let (tag, payload) = match input {
            Input::Null => (Tag::Null, 0),
            Input::Bool(v) => (Tag::Bool, v as u64),
            Input::Int(v) => (Tag::Int, v as u64),
            // The baseline needs no NaN tag reservation and preserves raw bits.
            Input::Float(v) => (Tag::Float, v.to_bits()),
            Input::Object(payload) => (Tag::Object, heap.objects.insert(Object { payload }).bits()),
        };
        Self { tag, payload }
    }
    fn kind(self) -> Kind {
        match self.tag {
            Tag::Null => Kind::Null,
            Tag::Bool => Kind::Bool,
            Tag::Int => Kind::Int,
            Tag::Float => Kind::Float,
            Tag::Object => Kind::Object,
        }
    }
    fn boolean(self) -> bool {
        debug_assert_eq!(self.tag, Tag::Bool);
        self.payload != 0
    }
    fn integer(self, _: &Heap) -> i64 {
        debug_assert_eq!(self.tag, Tag::Int);
        self.payload as i64
    }
    fn float(self) -> f64 {
        debug_assert_eq!(self.tag, Tag::Float);
        f64::from_bits(self.payload)
    }
    fn object(self) -> Handle {
        debug_assert_eq!(self.tag, Tag::Object);
        Handle(self.payload)
    }
    fn release(self, heap: &mut Heap) {
        if self.tag == Tag::Object {
            assert!(heap.objects.remove(self.object()));
        }
    }
}

impl NanBox {
    fn tag(self) -> u64 {
        self.0 & !PAYLOAD_MASK
    }
    fn payload(self) -> u64 {
        self.0 & PAYLOAD_MASK
    }
}

impl Representation for NanBox {
    const NAME: &'static str = "nanbox8";
    fn encode(input: Input, heap: &mut Heap) -> Self {
        Self(match input {
            Input::Null => NULL,
            Input::Bool(v) => BOOL | v as u64,
            Input::Int(v) if (SMALL_MIN..=SMALL_MAX).contains(&v) => {
                INT | ((v as u64) & PAYLOAD_MASK)
            }
            Input::Int(v) => WIDE | heap.wide.insert(v).bits(),
            Input::Float(v) => normalized_float_bits(v),
            Input::Object(payload) => OBJECT | heap.objects.insert(Object { payload }).bits(),
        })
    }
    fn kind(self) -> Kind {
        match self.tag() {
            NULL => Kind::Null,
            BOOL => Kind::Bool,
            INT | WIDE => Kind::Int,
            OBJECT => Kind::Object,
            _ => Kind::Float,
        }
    }
    fn boolean(self) -> bool {
        debug_assert_eq!(self.tag(), BOOL);
        self.payload() != 0
    }
    fn integer(self, heap: &Heap) -> i64 {
        if self.tag() == WIDE {
            *heap
                .wide
                .get(Handle(self.payload()))
                .expect("invalid wide handle")
        } else {
            debug_assert_eq!(self.tag(), INT);
            ((self.payload() << 16) as i64) >> 16
        }
    }
    fn float(self) -> f64 {
        debug_assert_eq!(self.kind(), Kind::Float);
        f64::from_bits(self.0)
    }
    fn object(self) -> Handle {
        debug_assert_eq!(self.tag(), OBJECT);
        Handle(self.payload())
    }
    fn release(self, heap: &mut Heap) {
        match self.tag() {
            OBJECT => assert!(heap.objects.remove(self.object())),
            WIDE => assert!(heap.wide.remove(Handle(self.payload()))),
            _ => {}
        }
    }
}

// Checked by target compilation too, with no dependence on native pointers.
const _: () = assert!(size_of::<Explicit>() == 16);
const _: () = assert!(size_of::<NanBox>() == 8);
const _: () = assert!(size_of::<Handle>() == 8);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<Slot<i64>>() == 12);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Slot<i64>>() == 16);

pub fn slot_size() -> usize {
    size_of::<Slot<i64>>()
}

/// A logical-result checksum, independent of representation and handle identity.
pub fn input_digest(input: Input) -> u64 {
    match input {
        Input::Null => 0x91,
        Input::Bool(v) => 0x200 | v as u64,
        Input::Int(v) => (v as u64).rotate_left(7) ^ 0x300,
        Input::Float(v) => normalized_float_bits(v).rotate_left(13) ^ 0x400,
        Input::Object(v) => v.rotate_left(17) ^ 0x500,
    }
}

pub fn value_digest<V: Representation>(value: V, heap: &Heap) -> u64 {
    match value.kind() {
        Kind::Null => input_digest(Input::Null),
        Kind::Bool => input_digest(Input::Bool(value.boolean())),
        Kind::Int => input_digest(Input::Int(value.integer(heap))),
        Kind::Float => input_digest(Input::Float(value.float())),
        Kind::Object => input_digest(Input::Object(
            heap.objects
                .get(value.object())
                .expect("invalid object")
                .payload,
        )),
    }
}

pub fn random_word(state: &mut u64) -> u64 {
    // Deterministic xorshift64*; no benchmark dependency or external entropy.
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    state.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

pub fn check_roundtrip(input: Input) {
    let mut heap = Heap::default();
    let a = Explicit::encode(input, &mut heap);
    let b = NanBox::encode(input, &mut heap);
    assert_eq!(a.kind(), b.kind());
    // Direct payload checks supplement the checksum (no checksum-only proof).
    match input {
        Input::Null => assert_eq!(a.kind(), Kind::Null),
        Input::Bool(v) => {
            assert_eq!(a.boolean(), v);
            assert_eq!(b.boolean(), v);
        }
        Input::Int(v) => {
            assert_eq!(a.integer(&heap), v);
            assert_eq!(b.integer(&heap), v);
        }
        Input::Float(v) => {
            assert_eq!(a.float().to_bits(), v.to_bits());
            assert_eq!(b.float().to_bits(), normalized_float_bits(v));
        }
        Input::Object(v) => {
            assert_eq!(heap.objects.get(a.object()).unwrap().payload, v);
            assert_eq!(heap.objects.get(b.object()).unwrap().payload, v);
        }
    }
    assert_eq!(value_digest(a, &heap), input_digest(input));
    assert_eq!(value_digest(b, &heap), input_digest(input));
    a.release(&mut heap);
    b.release(&mut heap);
    assert_eq!(heap.objects.storage().live, 0);
    assert_eq!(heap.wide.storage().live, 0);
}

/// Always runs before timing, as well as under cargo test.
pub fn validate_boundaries() {
    for input in [Input::Null, Input::Bool(false), Input::Bool(true)] {
        check_roundtrip(input);
    }
    for value in [
        i64::MIN,
        i64::MIN + 1,
        SMALL_MIN - 1,
        SMALL_MIN,
        SMALL_MIN + 1,
        -1,
        0,
        1,
        SMALL_MAX - 1,
        SMALL_MAX,
        SMALL_MAX + 1,
        i64::MAX - 1,
        i64::MAX,
    ] {
        check_roundtrip(Input::Int(value));
    }
    for bits in [
        0,
        1 << 63,
        1,
        (1 << 63) | 1,
        0x0010_0000_0000_0000,
        0x7fef_ffff_ffff_ffff,
        0xffef_ffff_ffff_ffff,
        0x7ff0_0000_0000_0000,
        0xfff0_0000_0000_0000,
        0x7ff0_0000_0000_0001,
        0xfff0_0000_0000_0001,
        CANONICAL_NAN,
        NULL,
        BOOL | 1,
        INT | PAYLOAD_MASK,
        OBJECT | PAYLOAD_MASK,
        WIDE | PAYLOAD_MASK,
        0x7fff_ffff_ffff_ffff,
        0xffff_ffff_ffff_ffff,
    ] {
        check_roundtrip(Input::Float(f64::from_bits(bits)));
    }
    for index in [0, 1, FIELD_MAX] {
        for generation in [1, 2, FIELD_MAX] {
            let h = Handle::new(index, generation).unwrap();
            assert_eq!(h.index(), index);
            assert_eq!(h.generation(), generation);
            let a = Explicit {
                tag: Tag::Object,
                payload: h.bits(),
            };
            let b = NanBox(OBJECT | h.bits());
            assert_eq!(a.object(), h);
            assert_eq!(b.object(), h);
            // Wide-int handles share the same packed payload boundary.
            assert_eq!(Handle(NanBox(WIDE | h.bits()).payload()), h);
        }
    }
    assert!(Handle::new(FIELD_MAX + 1, 1).is_none());
    assert!(Handle::new(0, 0).is_none());
    assert!(Handle::new(0, FIELD_MAX + 1).is_none());
    check_roundtrip(Input::Object(u64::MAX));
    let mut arena = Arena::default();
    let first = arena.insert(i64::MIN);
    assert!(arena.remove(first));
    assert!(!arena.remove(first));
    assert!(arena.get(first).is_none());
    let second = arena.insert(i64::MAX);
    assert_eq!(first.index(), second.index());
    assert_ne!(first.generation(), second.generation());
    assert_eq!(arena.get(second), Some(&i64::MAX));
    assert!(arena.get(Handle::new(FIELD_MAX, 1).unwrap()).is_none());
    // Reach exhaustion directly instead of performing 16 million allocations.
    arena.slots[second.index() as usize].generation = FIELD_MAX;
    let last = Handle::new(second.index(), FIELD_MAX).unwrap();
    assert!(arena.remove(last));
    assert!(arena.get(last).is_none());
    let third = arena.insert(42);
    assert_ne!(last.index(), third.index());
    assert_eq!(third.generation(), 1);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boundaries_and_retired_generations() {
        validate_boundaries();
    }
    #[test]
    fn deterministic_bit_pattern_roundtrips() {
        let mut state = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..100_000 {
            let bits = random_word(&mut state);
            check_roundtrip(Input::Int(bits as i64));
            check_roundtrip(Input::Float(f64::from_bits(bits)));
            check_roundtrip(Input::Object(bits));
        }
    }
    #[test]
    fn wide_allocation_is_exactly_outside_signed_48_bits() {
        let mut heap = Heap::default();
        for v in [SMALL_MIN, SMALL_MAX] {
            let value = NanBox::encode(Input::Int(v), &mut heap);
            assert_eq!(heap.wide.storage().live, 0);
            value.release(&mut heap);
        }
        for v in [SMALL_MIN - 1, SMALL_MAX + 1, i64::MIN, i64::MAX] {
            let value = NanBox::encode(Input::Int(v), &mut heap);
            assert_eq!(heap.wide.storage().live, 1);
            assert_eq!(heap.wide.storage().box_bytes, 8);
            value.release(&mut heap);
            assert_eq!(heap.wide.storage().live, 0);
        }
    }
}
