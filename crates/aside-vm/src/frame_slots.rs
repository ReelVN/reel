//! Continuous root windows. All publishing writes are sealed behind Heap's barrier.
use crate::root_ranges::{ChargedVec, Reservation};
use crate::slots::{RangeKey, index};
use crate::{
    Heap, HeapBudgetUsage, HeapError, HeapId, RootKind, RootOwner, RootRangeToken, RootToken,
    SessionEpoch, Value,
};
mod growth;

#[derive(Clone, Copy, Debug)]
pub struct FrameSlotsLimits {
    pub initial_capacity: u32,
    pub max_capacity: u32,
    pub max_window_len: u32,
}
#[derive(Clone, Copy, Debug)]
pub struct RangePosition {
    pub range: RootRangeToken,
    pub index: u32,
}
#[derive(Clone, Copy)]
struct Window {
    key: RangeKey,
    base: u32,
    len: u32,
}
#[derive(Clone, Copy)]
struct Span {
    base: u32,
    len: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FrameSlotsStats {
    pub capacity: u32,
    pub active_values: u32,
    pub active_ranges: u32,
    pub free_spans: u32,
    pub value_bytes: u64,
    pub bitmap_bytes: u64,
    pub metadata_bytes: u64,
    pub charged_bytes: u64,
}
/// Mapping/copy sizes and memory residency, not an exact total-work count.
/// See [`FrameSlots::relocate`] for the additional work performed.
#[derive(Clone, Copy, Debug, Default)]
pub struct RelocationCost {
    /// Mapping size, including holes and the sentinel; not total index inspections.
    pub descriptor_slots: u32,
    /// Value positions copied from registered windows, including uninitialized nulls.
    /// Does not include null initialization of the full replacement capacity.
    pub values: u32,
    pub bitmap_bytes: u64,
    pub old_buffer_bytes: u64,
    pub new_buffer_bytes: u64,
    pub peak_charged_bytes: u64,
}
/// A synchronous operation prepares one contiguous span for all of its windows.
/// The plan is derived from scalar pool statistics and owns no borrowed values.
pub(crate) struct WindowAdmission {
    pub capacity: Option<u32>,
    pub work: u64,
    pub allocation_bytes: u64,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct RangeReleaseCursor {
    heap: Option<HeapId>,
    epoch: Option<SessionEpoch>,
    attachment: Option<u64>,
    owner: Option<RootOwner>,
    next: u32,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct RangeReleaseProgress {
    /// Descriptor indices inspected, including holes. Excludes free-span Vec
    /// insertion/coalescing, whose work per released range is bounded by the
    /// configured range capacity; this is not a total-work count.
    pub slots: u32,
    pub ranges: u32,
    pub complete: bool,
}

/// The VM owns this pool beside its Heap. A token alone is not a new registration.
/// A borrowed argument view cannot cross a relocation or reentrant write:
/// ```compile_fail,E0502
/// use aside_vm::{FrameSlots, Heap, RootRangeToken};
/// fn move_with_a_live_view(heap: &mut Heap, frames: &mut FrameSlots, range: RootRangeToken) {
///     let view = frames.borrow_initialized(range).unwrap();
///     frames.relocate(heap, 64).unwrap();
///     std::hint::black_box(view);
/// }
/// ```
pub struct FrameSlots {
    values: Option<ChargedVec<Value>>,
    bitmap: Option<ChargedVec<u64>>,
    mappings: Option<ChargedVec<Option<Window>>>,
    free: Option<ChargedVec<Span>>,
    usage: HeapBudgetUsage,
    heap: HeapId,
    epoch: SessionEpoch,
    attachment: u64,
    limits: FrameSlotsLimits,
    active_values: u32,
    active_ranges: u32,
    largest_free: u32,
    closed: bool,
    // Keep the bounded inline control metadata charged until this owner drops.
    control_charge: Reservation,
}

/// A short-lived read-only provider. There is no public implementation/registration escape hatch.
#[derive(Clone, Copy)]
pub struct RootRanges<'a> {
    slots: &'a FrameSlots,
}
impl<'a> RootRanges<'a> {
    pub(crate) fn check_heap(&self, heap: &Heap) -> Result<(), HeapError> {
        self.slots.check_heap(heap)
    }
    pub(crate) fn resolve(&self, token: RootRangeToken) -> Result<&'a [Value], HeapError> {
        let w = self.slots.window(token)?;
        let start = w.base as usize;
        Ok(&self.slots.values.as_ref().unwrap().data[start..start + w.len as usize])
    }
}

impl FrameSlots {
    pub fn new(heap: &mut Heap, limits: FrameSlotsLimits) -> Result<Self, HeapError> {
        if limits.max_capacity == 0
            || limits.max_window_len == 0
            || limits.initial_capacity > limits.max_capacity
            || limits.max_window_len > limits.max_capacity
            || limits.max_capacity as u64 * std::mem::size_of::<Value>() as u64 > isize::MAX as u64
        {
            return Err(HeapError::InvalidLimits);
        }
        if heap.usage.0.attachment.get().is_some() {
            if heap.usage.0.provider_alive.get() {
                return Err(HeapError::RootProviderAttached);
            }
            if heap.ranges.occupied() != 0 {
                return Err(HeapError::MissingRootProvider);
            }
        }
        let attachment = heap
            .next_attachment
            .checked_add(1)
            .ok_or(HeapError::IdentityExhausted)?;
        let usage = heap.budget_usage();
        let control_charge = Reservation::new(&usage, std::mem::size_of::<Self>() as u64)?;
        let mut values = ChargedVec::new(&usage, limits.initial_capacity as usize)?;
        values
            .data
            .resize(limits.initial_capacity as usize, Value::NULL);
        let mut bitmap = ChargedVec::new(&usage, (limits.initial_capacity as usize).div_ceil(64))?;
        bitmap
            .data
            .resize((limits.initial_capacity as usize).div_ceil(64), 0);
        let slots = heap.range_index.len();
        let mut mappings = ChargedVec::new(&usage, slots)?;
        mappings.data.resize(slots, None);
        let mut free = ChargedVec::new(&usage, slots)?;
        if limits.initial_capacity != 0 {
            free.data.push(Span {
                base: 0,
                len: limits.initial_capacity,
            });
        }
        // Nothing above published a descriptor or attachment; errors own/drop their buffers.
        heap.next_attachment = attachment;
        usage.0.attachment.set(Some(attachment));
        usage.0.provider_alive.set(true);
        Ok(Self {
            values: Some(values),
            bitmap: Some(bitmap),
            mappings: Some(mappings),
            free: Some(free),
            usage,
            heap: heap.id(),
            epoch: heap.epoch(),
            attachment,
            limits,
            active_values: 0,
            active_ranges: 0,
            largest_free: limits.initial_capacity,
            closed: false,
            control_charge,
        })
    }
    fn attached(&self) -> Result<(), HeapError> {
        if !self.usage.0.heap_alive.get() {
            return Err(HeapError::HeapUnavailable);
        }
        if self.closed || self.usage.0.attachment.get() != Some(self.attachment) {
            return Err(HeapError::WrongRootProvider);
        }
        if !self.usage.0.provider_alive.get() {
            return Err(HeapError::MissingRootProvider);
        }
        Ok(())
    }
    pub(crate) fn check_heap(&self, heap: &Heap) -> Result<(), HeapError> {
        if self.heap != heap.id() {
            return Err(HeapError::WrongHeap);
        }
        if self.epoch != heap.epoch() {
            return Err(HeapError::WrongEpoch);
        }
        self.attached()
    }
    fn window(&self, token: RootRangeToken) -> Result<Window, HeapError> {
        if token.heap != self.heap {
            return Err(HeapError::WrongHeap);
        }
        if token.epoch != self.epoch {
            return Err(HeapError::WrongEpoch);
        }
        self.attached()?;
        self.mappings
            .as_ref()
            .unwrap()
            .data
            .get(index(token.key))
            .copied()
            .flatten()
            .filter(|w| w.key == token.key)
            .ok_or(HeapError::InvalidRootRange)
    }
    fn position(&self, at: RangePosition) -> Result<usize, HeapError> {
        let w = self.window(at.range)?;
        if at.index >= w.len {
            return Err(HeapError::OutOfBounds);
        }
        Ok((w.base + at.index) as usize)
    }
    fn initialized(&self, i: usize) -> bool {
        self.bitmap.as_ref().unwrap().data[i / 64] & (1 << (i % 64)) != 0
    }
    fn set_initialized(&mut self, i: usize, set: bool) {
        let word = &mut self.bitmap.as_mut().unwrap().data[i / 64];
        let bit = 1 << (i % 64);
        if set {
            *word |= bit;
        } else {
            *word &= !bit;
        }
    }
    fn clear_initialized(&mut self, start: usize, len: usize) {
        if len == 0 {
            return;
        }
        let end = start + len - 1;
        let first = start / 64;
        let last = end / 64;
        let from = u64::MAX << (start % 64);
        let through = u64::MAX >> (63 - end % 64);
        let words = &mut self.bitmap.as_mut().unwrap().data;
        if first == last {
            words[first] &= !(from & through);
        } else {
            words[first] &= !from;
            words[first + 1..last].fill(0);
            words[last] &= !through;
        }
    }
    fn put(&mut self, i: usize, value: Value, initialized: bool) {
        self.values.as_mut().unwrap().data[i] = value;
        self.set_initialized(i, initialized);
    }
    pub fn roots(&self) -> RootRanges<'_> {
        RootRanges { slots: self }
    }
    pub fn range_owner(
        &self,
        heap: &Heap,
        range: RootRangeToken,
    ) -> Result<(RootOwner, RootKind), HeapError> {
        self.check_heap(heap)?;
        self.window(range)?;
        let d = heap.range_descriptor(range)?;
        Ok((d.owner, d.kind))
    }
    pub fn open(
        &mut self,
        heap: &mut Heap,
        owner: RootOwner,
        kind: RootKind,
        len: u32,
    ) -> Result<RootRangeToken, HeapError> {
        self.open_with_limit(heap, owner, kind, len, self.limits.max_window_len)
    }
    /// Loaded module roots are bounded by pool residency, not a single execution
    /// operation. Opening them happens during explicit package admission.
    pub(crate) fn open_module_set(
        &mut self,
        heap: &mut Heap,
        owner: RootOwner,
        len: u32,
    ) -> Result<RootRangeToken, HeapError> {
        self.open_with_limit(
            heap,
            owner,
            RootKind::ModuleSet,
            len,
            self.limits.max_capacity,
        )
    }
    fn open_with_limit(
        &mut self,
        heap: &mut Heap,
        owner: RootOwner,
        kind: RootKind,
        len: u32,
        limit: u32,
    ) -> Result<RootRangeToken, HeapError> {
        self.check_heap(heap)?;
        if len > limit {
            return Err(HeapError::OperationLimit {
                requested: len as u64,
                limit: limit as u64,
            });
        }
        heap.ensure_range_slot()?;
        self.grow_descriptors(heap.range_index.len())?;
        let free_index = if len == 0 {
            None
        } else {
            Some(
                self.free
                    .as_ref()
                    .unwrap()
                    .data
                    .iter()
                    .position(|s| s.len >= len)
                    .ok_or(HeapError::WindowCapacity)?,
            )
        };
        let base = free_index.map_or(0, |i| self.free.as_ref().unwrap().data[i].base);
        // Whole null contents and clear initialization bits precede joint publication.
        self.values.as_mut().unwrap().data[base as usize..(base + len) as usize].fill(Value::NULL);
        self.clear_initialized(base as usize, len as usize);
        let token = heap.register_null_range(owner, kind, len)?;
        self.mappings.as_mut().unwrap().data[index(token.key)] = Some(Window {
            key: token.key,
            base,
            len,
        });
        if let Some(i) = free_index {
            let spans = &mut self.free.as_mut().unwrap().data;
            let was_largest = spans[i].len == self.largest_free;
            spans[i].base += len;
            spans[i].len -= len;
            if spans[i].len == 0 {
                spans.remove(i);
            }
            if was_largest {
                self.largest_free = spans.iter().map(|span| span.len).max().unwrap_or(0);
            }
        }
        self.active_values += len;
        self.active_ranges += 1;
        Ok(token)
    }
    fn grow_descriptors(&mut self, slots: usize) -> Result<(), HeapError> {
        if slots <= self.mappings.as_ref().unwrap().data.len() {
            return Ok(());
        }
        let mut mappings = ChargedVec::new(&self.usage, slots)?;
        mappings
            .data
            .extend_from_slice(&self.mappings.as_ref().unwrap().data);
        mappings.data.resize(slots, None);
        let mut free = ChargedVec::new(&self.usage, slots)?;
        free.data
            .extend_from_slice(&self.free.as_ref().unwrap().data);
        // Both allocations succeeded. Existing windows and free spans retain
        // their positions; no descriptor can become visible between these swaps.
        self.mappings = Some(mappings);
        self.free = Some(free);
        Ok(())
    }
    /// Host task admission is an exclusive safe point. Retain execution room
    /// while adding pooled roots; ordinary instruction windows remain budgeted.
    pub(crate) fn reserve_task(&mut self, heap: &mut Heap, roots: u32) -> Result<(), HeapError> {
        let headroom = roots.saturating_add(self.limits.max_window_len);
        if self
            .free
            .as_ref()
            .unwrap()
            .data
            .iter()
            .any(|span| span.len >= headroom)
        {
            return Ok(());
        }
        let minimum = self
            .active_values
            .checked_add(roots)
            .ok_or(HeapError::WindowCapacity)?;
        let desired = self.active_values.saturating_add(headroom);
        let capacity = self
            .stats()
            .capacity
            .saturating_mul(2)
            .max(desired)
            .min(self.limits.max_capacity);
        if capacity < minimum {
            return Err(HeapError::WindowCapacity);
        }
        self.relocate(heap, capacity)?;
        Ok(())
    }
    pub(crate) fn window_admission(
        &self,
        heap: &Heap,
        windows: usize,
        values: u64,
    ) -> WindowAdmission {
        let (descriptors, work, bytes) = heap.range_admission(windows);
        let mut plan = WindowAdmission {
            capacity: None,
            work,
            allocation_bytes: bytes,
        };
        if descriptors > self.mappings.as_ref().unwrap().data.len() {
            plan.work += descriptors as u64
                + self.mappings.as_ref().unwrap().data.len() as u64
                + self.free.as_ref().unwrap().data.len() as u64;
            plan.allocation_bytes += descriptors as u64
                * (std::mem::size_of::<Option<Window>>() + std::mem::size_of::<Span>()) as u64;
        }
        if values <= u64::from(self.largest_free) {
            return plan;
        }
        let minimum = u64::from(self.active_values) + values;
        if minimum > u64::from(self.limits.max_capacity) {
            // Existing window admission reports the configured capacity limit.
            return plan;
        }
        let old_capacity = self.values.as_ref().unwrap().data.len() as u32;
        let capacity = if minimum <= u64::from(old_capacity) {
            old_capacity
        } else {
            old_capacity
                .saturating_mul(2)
                .max(minimum as u32)
                .min(self.limits.max_capacity)
        };
        let bitmap_words = u64::from(capacity).div_ceil(64);
        plan.capacity = Some(capacity);
        // Preflight resolves every live descriptor; relocation traverses the
        // mapping twice, initializes the entire replacement, and barriers/copies
        // each registered value and its initialization bit.
        plan.work += 8 * descriptors as u64
            + u64::from(capacity)
            + bitmap_words
            + 5 * u64::from(self.active_values)
            + self.free.as_ref().unwrap().data.len() as u64
            + 8;
        plan.allocation_bytes +=
            u64::from(capacity) * std::mem::size_of::<Value>() as u64 + bitmap_words * 8;
        plan
    }
    pub(crate) fn admit_windows(
        &mut self,
        heap: &mut Heap,
        windows: usize,
        values: u64,
    ) -> Result<(), HeapError> {
        let capacity = self.window_admission(heap, windows, values).capacity;
        heap.ensure_range_slots(windows)?;
        self.grow_descriptors(heap.range_index.len())?;
        if let Some(capacity) = capacity {
            self.relocate(heap, capacity)?;
        }
        Ok(())
    }
    pub fn read(&self, at: RangePosition) -> Result<Value, HeapError> {
        let i = self.position(at)?;
        if !self.initialized(i) {
            return Err(HeapError::UninitializedSlot);
        }
        Ok(self.values.as_ref().unwrap().data[i])
    }
    pub fn borrow_initialized(&self, range: RootRangeToken) -> Result<&[Value], HeapError> {
        let w = self.window(range)?;
        for i in w.base..w.base + w.len {
            if !self.initialized(i as usize) {
                return Err(HeapError::UninitializedSlot);
            }
        }
        self.roots().resolve(range)
    }
    /// Checks only the logical initialized view; unused null storage stays clear.
    pub fn borrow_initialized_window(
        &self,
        range: RootRangeToken,
        start: u32,
        len: u32,
    ) -> Result<&[Value], HeapError> {
        let w = self.window(range)?;
        let end = start
            .checked_add(len)
            .filter(|n| *n <= w.len)
            .ok_or(HeapError::OutOfBounds)?;
        for i in w.base + start..w.base + end {
            if !self.initialized(i as usize) {
                return Err(HeapError::UninitializedSlot);
            }
        }
        Ok(&self.roots().resolve(range)?[start as usize..end as usize])
    }
    pub fn write(
        &mut self,
        heap: &mut Heap,
        at: RangePosition,
        value: Value,
    ) -> Result<(), HeapError> {
        self.check_heap(heap)?;
        let i = self.position(at)?;
        heap.root_insertion_barrier(value)?;
        self.put(i, value, true);
        Ok(())
    }
    pub fn clear(&mut self, heap: &mut Heap, at: RangePosition) -> Result<(), HeapError> {
        self.check_heap(heap)?;
        let i = self.position(at)?;
        heap.root_insertion_barrier(Value::NULL)?;
        self.put(i, Value::NULL, false);
        Ok(())
    }
    pub fn copy_value(
        &mut self,
        heap: &mut Heap,
        from: RangePosition,
        to: RangePosition,
    ) -> Result<(), HeapError> {
        self.check_heap(heap)?;
        let value = self.read(from)?;
        self.write(heap, to, value)
    }
    pub fn move_value(
        &mut self,
        heap: &mut Heap,
        from: RangePosition,
        to: RangePosition,
    ) -> Result<(), HeapError> {
        self.check_heap(heap)?;
        let value = self.read(from)?;
        let source = self.position(from)?;
        let target = self.position(to)?;
        heap.root_insertion_barrier(value)?;
        if source != target {
            self.put(target, value, true);
            self.put(source, Value::NULL, false);
        }
        Ok(())
    }
    pub fn swap(
        &mut self,
        heap: &mut Heap,
        a: RangePosition,
        b: RangePosition,
    ) -> Result<(), HeapError> {
        self.check_heap(heap)?;
        let a_i = self.position(a)?;
        let b_i = self.position(b)?;
        let a_set = self.initialized(a_i);
        let b_set = self.initialized(b_i);
        let a_v = self.values.as_ref().unwrap().data[a_i];
        let b_v = self.values.as_ref().unwrap().data[b_i];
        heap.root_insertion_barrier(a_v)?;
        heap.root_insertion_barrier(b_v)?;
        self.put(a_i, b_v, b_set);
        self.put(b_i, a_v, a_set);
        Ok(())
    }
    pub fn move_to_host_root(
        &mut self,
        heap: &mut Heap,
        from: RangePosition,
        owner: RootOwner,
    ) -> Result<RootToken, HeapError> {
        self.check_heap(heap)?;
        let value = self.read(from)?;
        let i = self.position(from)?;
        let token = heap.add_root(owner, RootKind::Host, value)?;
        self.put(i, Value::NULL, false);
        Ok(token)
    }
    fn recycle(&mut self, span: Span) {
        if span.len == 0 {
            return;
        }
        let spans = &mut self.free.as_mut().unwrap().data;
        let i = spans.partition_point(|s| s.base < span.base);
        assert!(spans.len() < spans.capacity());
        spans.insert(i, span);
        if i + 1 < spans.len() && spans[i].base + spans[i].len == spans[i + 1].base {
            let right = spans.remove(i + 1);
            spans[i].len += right.len;
        }
        if i > 0 && spans[i - 1].base + spans[i - 1].len == spans[i].base {
            let right = spans.remove(i);
            spans[i - 1].len += right.len;
            self.largest_free = self.largest_free.max(spans[i - 1].len);
        } else {
            self.largest_free = self.largest_free.max(spans[i].len);
        }
    }
    /// Unregister a range and recycle its span while retaining pool capacity.
    /// Recycling may shift/coalesce O(max_root_ranges) free-span Vec entries;
    /// cancellation's descriptor-index count does not include that work.
    pub fn release(&mut self, heap: &mut Heap, range: RootRangeToken) -> Result<(), HeapError> {
        self.check_heap(heap)?;
        let w = self.window(range)?;
        let d = heap.range_descriptor(range)?;
        if d.len != w.len {
            return Err(HeapError::RootRangeLength {
                expected: d.len,
                actual: w.len,
            });
        }
        heap.unregister_range(range)?;
        self.mappings.as_mut().unwrap().data[index(range.key)] = None;
        self.active_values -= w.len;
        self.active_ranges -= 1;
        self.recycle(Span {
            base: w.base,
            len: w.len,
        });
        Ok(())
    }
    /// Inspect at most `slots` descriptor indices and revoke this owner's ranges.
    /// Each revocation also calls [`Self::release`] and maintains the free-span
    /// Vec. [`RangeReleaseProgress::slots`] excludes that work, so the quota
    /// bounds index inspections rather than total work or wall-clock time.
    pub fn release_owner_step(
        &mut self,
        heap: &mut Heap,
        owner: RootOwner,
        cursor: &mut RangeReleaseCursor,
        slots: u32,
    ) -> Result<RangeReleaseProgress, HeapError> {
        self.check_heap(heap)?;
        if let Some(a) = cursor.attachment {
            if cursor.heap != Some(self.heap) {
                return Err(HeapError::WrongHeap);
            }
            if cursor.epoch != Some(self.epoch) {
                return Err(HeapError::WrongEpoch);
            }
            if a != self.attachment {
                return Err(HeapError::WrongRootProvider);
            }
            if cursor.owner != Some(owner) {
                return Err(HeapError::InvalidRangeCursor);
            }
        } else {
            cursor.heap = Some(self.heap);
            cursor.epoch = Some(self.epoch);
            cursor.attachment = Some(self.attachment);
            cursor.owner = Some(owner);
        }
        let end = cursor
            .next
            .saturating_add(slots)
            .min(heap.range_scan_len as u32);
        let mut p = RangeReleaseProgress::default();
        while cursor.next < end {
            if let Some(key) = heap.range_index[cursor.next as usize] {
                let token = RootRangeToken::new(heap, key);
                if heap.range_descriptor(token)?.owner == owner {
                    self.release(heap, token)?;
                    p.ranges += 1;
                }
            }
            cursor.next += 1;
            p.slots += 1;
        }
        p.complete = cursor.next == heap.range_scan_len as u32;
        Ok(p)
    }
    // Close/relocation preflight: errors preserve every descriptor and allocation.
    fn preflight(&self, heap: &Heap) -> Result<(), HeapError> {
        self.check_heap(heap)?;
        let mut count = 0;
        for &key in heap.range_index.iter().flatten() {
            let token = RootRangeToken::new(heap, key);
            let w = self.window(token)?;
            let d = heap.range_descriptor(token)?;
            if w.len != d.len {
                return Err(HeapError::RootRangeLength {
                    expected: d.len,
                    actual: w.len,
                });
            }
            count += 1;
        }
        if count != self.active_ranges {
            return Err(HeapError::InvalidRootRange);
        }
        Ok(())
    }
    /// Relocate at an exclusive safe point, preserving logical windows and bits.
    /// [`RelocationCost`] reports mapping size and registered-window copy volume.
    /// Actual work also includes descriptor preflight, two full mapping passes,
    /// null initialization of all `capacity` Values, zeroing the entire new
    /// bitmap, per-Value barriers/bit copying, and releasing replaced buffers.
    /// Its `descriptor_slots` and `values` fields are not exact total-work counts.
    pub fn relocate(
        &mut self,
        heap: &mut Heap,
        capacity: u32,
    ) -> Result<RelocationCost, HeapError> {
        self.relocate_growing(heap, capacity, None)
    }
    fn relocate_growing(
        &mut self,
        heap: &mut Heap,
        capacity: u32,
        growth: Option<(RangeKey, u32)>,
    ) -> Result<RelocationCost, HeapError> {
        self.preflight(heap)?;
        let delta = growth.map_or(0, |(key, len)| {
            len - self.mappings.as_ref().unwrap().data[index(key)]
                .unwrap()
                .len
        });
        if capacity < self.active_values.saturating_add(delta)
            || capacity > self.limits.max_capacity
        {
            return Err(HeapError::WindowCapacity);
        }
        let mut new_values = ChargedVec::new(&self.usage, capacity as usize)?;
        new_values.data.resize(capacity as usize, Value::NULL);
        let mut new_bitmap = ChargedVec::new(&self.usage, (capacity as usize).div_ceil(64))?;
        new_bitmap.data.resize((capacity as usize).div_ceil(64), 0);
        let mut next = 0;
        for w in self.mappings.as_ref().unwrap().data.iter().flatten() {
            for n in 0..w.len {
                let old = (w.base + n) as usize;
                let to = (next + n) as usize;
                let value = self.values.as_ref().unwrap().data[old];
                heap.root_insertion_barrier(value)?;
                new_values.data[to] = value;
                if self.initialized(old) {
                    new_bitmap.data[to / 64] |= 1 << (to % 64);
                }
            }
            next += growth
                .filter(|(key, _)| *key == w.key)
                .map_or(w.len, |(_, len)| len);
        }
        let cost = RelocationCost {
            descriptor_slots: self.mappings.as_ref().unwrap().data.len() as u32,
            values: self.active_values,
            bitmap_bytes: new_bitmap.bytes(),
            old_buffer_bytes: self.values.as_ref().unwrap().bytes()
                + self.bitmap.as_ref().unwrap().bytes(),
            new_buffer_bytes: new_values.bytes() + new_bitmap.bytes(),
            peak_charged_bytes: self.usage.peak_charged_bytes(),
        };
        next = 0;
        for w in self.mappings.as_mut().unwrap().data.iter_mut().flatten() {
            w.base = next;
            if let Some((key, len)) = growth
                && w.key == key
            {
                w.len = len;
                heap.ranges.get_mut(key).unwrap().len = len;
            }
            next += w.len;
        }
        self.active_values += delta;
        let old_values = self.values.replace(new_values);
        let old_bitmap = self.bitmap.replace(new_bitmap);
        let spans = &mut self.free.as_mut().unwrap().data;
        spans.clear();
        if next < capacity {
            spans.push(Span {
                base: next,
                len: capacity - next,
            });
        }
        self.largest_free = capacity - next;
        drop(old_values);
        drop(old_bitmap);
        Ok(cost)
    }
    pub fn close(&mut self, heap: &mut Heap) -> Result<(), HeapError> {
        if self.heap != heap.id() {
            return Err(HeapError::WrongHeap);
        }
        if self.epoch != heap.epoch() {
            return Err(HeapError::WrongEpoch);
        }
        if self.closed {
            return Ok(());
        }
        self.preflight(heap)?;
        // Preflight guarantees this phase cannot fail halfway and consume ownership.
        for position in 0..heap.range_index.len() {
            if let Some(key) = heap.range_index[position] {
                heap.unregister_range(RootRangeToken::new(heap, key))
                    .expect("preflighted descriptor");
            }
        }
        self.active_values = 0;
        self.active_ranges = 0;
        self.closed = true;
        drop(self.values.take());
        drop(self.bitmap.take());
        drop(self.mappings.take());
        drop(self.free.take());
        self.usage.0.provider_alive.set(false);
        self.usage.0.attachment.set(None);
        Ok(())
    }
    pub fn stats(&self) -> FrameSlotsStats {
        let value_bytes = self.values.as_ref().map_or(0, ChargedVec::bytes);
        let bitmap_bytes = self.bitmap.as_ref().map_or(0, ChargedVec::bytes);
        let metadata_bytes = self.control_charge.bytes()
            + self.mappings.as_ref().map_or(0, ChargedVec::bytes)
            + self.free.as_ref().map_or(0, ChargedVec::bytes);
        FrameSlotsStats {
            capacity: self.values.as_ref().map_or(0, |v| v.data.len() as u32),
            active_values: self.active_values,
            active_ranges: self.active_ranges,
            free_spans: self.free.as_ref().map_or(0, |v| v.data.len() as u32),
            value_bytes,
            bitmap_bytes,
            metadata_bytes,
            charged_bytes: value_bytes + bitmap_bytes + metadata_bytes,
        }
    }
}
impl Drop for FrameSlots {
    fn drop(&mut self) {
        if !self.closed && self.usage.0.attachment.get() == Some(self.attachment) {
            // The heap keeps descriptors; any subsequent collection fails explicitly.
            self.usage.0.provider_alive.set(false);
        }
        // ChargedVec fields deallocate then refund, before inline control_charge drops.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GcBudget, GcPhase, HeapLimits};

    #[test]
    fn recycled_windows_clear_only_their_initialization_bits_at_word_boundaries() {
        for prefix in [0, 1, 17, 63, 64, 65] {
            for length in [0, 1, 2, 63, 64, 65, 127, 128, 523] {
                let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
                let mut slots = FrameSlots::new(
                    &mut heap,
                    FrameSlotsLimits {
                        initial_capacity: 1024,
                        max_capacity: 1024,
                        max_window_len: 1024,
                    },
                )
                .unwrap();
                let left = slots
                    .open(&mut heap, RootOwner(1), RootKind::TaskFrame, prefix)
                    .unwrap();
                let middle = slots
                    .open(&mut heap, RootOwner(1), RootKind::TaskFrame, length)
                    .unwrap();
                let right = slots
                    .open(&mut heap, RootOwner(1), RootKind::TaskFrame, 65)
                    .unwrap();
                for (range, len) in [(left, prefix), (middle, length), (right, 65)] {
                    for index in 0..len {
                        slots
                            .write(&mut heap, RangePosition { range, index }, Value::int(42))
                            .unwrap();
                    }
                }
                slots.release(&mut heap, middle).unwrap();
                let reused = slots
                    .open(&mut heap, RootOwner(1), RootKind::TaskFrame, length)
                    .unwrap();
                for (range, len) in [(left, prefix), (right, 65)] {
                    for index in 0..len {
                        assert_eq!(
                            slots.read(RangePosition { range, index }).unwrap().as_int(),
                            Some(42)
                        );
                    }
                }
                for index in 0..length {
                    assert!(matches!(
                        slots.read(RangePosition {
                            range: reused,
                            index
                        }),
                        Err(HeapError::UninitializedSlot)
                    ));
                }
                assert!(
                    slots
                        .roots()
                        .resolve(reused)
                        .unwrap()
                        .iter()
                        .all(|value| value.same_bits(Value::NULL))
                );
                slots.close(&mut heap).unwrap();
            }
        }
    }

    #[test]
    #[ignore = "root-window admission cost for script and native call sizes"]
    fn root_window_admission_cost() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let mut slots = FrameSlots::new(
            &mut heap,
            FrameSlotsLimits {
                initial_capacity: 8192,
                max_capacity: 8192,
                max_window_len: 4096,
            },
        )
        .unwrap();
        let prefix = slots
            .open(&mut heap, RootOwner(1), RootKind::Arguments, 17)
            .unwrap();
        for length in [3, 64, 523, 4096] {
            let started = std::time::Instant::now();
            for _ in 0..10_000 {
                let range = slots
                    .open(
                        &mut heap,
                        RootOwner(1),
                        RootKind::NativeContinuation,
                        length,
                    )
                    .unwrap();
                slots
                    .write(
                        &mut heap,
                        RangePosition {
                            range,
                            index: length - 1,
                        },
                        Value::int(42),
                    )
                    .unwrap();
                std::hint::black_box(
                    slots
                        .read(RangePosition {
                            range,
                            index: length - 1,
                        })
                        .unwrap(),
                );
                slots.release(&mut heap, range).unwrap();
            }
            println!(
                "root_windows length={length} iterations=10000 elapsed_ns={} charged_bytes={}",
                started.elapsed().as_nanos(),
                slots.stats().charged_bytes
            );
        }
        slots.release(&mut heap, prefix).unwrap();
        slots.close(&mut heap).unwrap();
    }
    #[test]
    fn task_admission_compacts_fragmented_free_windows_without_losing_roots() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let mut slots = FrameSlots::new(
            &mut heap,
            FrameSlotsLimits {
                initial_capacity: 128,
                max_capacity: 256,
                max_window_len: 64,
            },
        )
        .unwrap();
        let ranges = (0..4)
            .map(|_| {
                slots
                    .open(&mut heap, RootOwner(1), RootKind::TaskFrame, 32)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let object = heap.alloc_cell(Value::int(73)).unwrap();
        let retained = RangePosition {
            range: ranges[1],
            index: 31,
        };
        slots.write(&mut heap, retained, object).unwrap();
        slots.release(&mut heap, ranges[0]).unwrap();
        slots.release(&mut heap, ranges[2]).unwrap();
        assert_eq!(slots.stats().capacity - slots.stats().active_values, 64);
        assert_eq!(
            slots.open(&mut heap, RootOwner(2), RootKind::Arguments, 64),
            Err(HeapError::WindowCapacity)
        );
        slots.reserve_task(&mut heap, 0).unwrap();
        let admitted = slots
            .open(&mut heap, RootOwner(2), RootKind::Arguments, 64)
            .unwrap();
        heap.begin_collection().unwrap();
        while heap.phase() != GcPhase::Idle {
            heap.gc_step_with_ranges(
                GcBudget {
                    units: 1,
                    ..GcBudget::default()
                },
                slots.roots(),
            )
            .unwrap();
        }
        assert!(slots.read(retained).unwrap().same_bits(object));
        assert_eq!(heap.cell_read(object).unwrap().as_int(), Some(73));
        slots.release(&mut heap, admitted).unwrap();
        slots.close(&mut heap).unwrap();
    }

    fn make() -> (Heap, FrameSlots) {
        let mut h = Heap::new(
            SessionEpoch(1),
            HeapLimits {
                max_objects: 4,
                max_roots: 2,
                max_root_ranges: 2,
                ..HeapLimits::default()
            },
        )
        .unwrap();
        let f = FrameSlots::new(
            &mut h,
            FrameSlotsLimits {
                initial_capacity: 8,
                max_capacity: 16,
                max_window_len: 8,
            },
        )
        .unwrap();
        (h, f)
    }
    #[test]
    fn missing_mapping_and_wrong_length_are_errors_and_close_error_retains_ownership() {
        let (mut h, mut f) = make();
        let r = f
            .open(&mut h, RootOwner(1), RootKind::TaskFrame, 1)
            .unwrap();
        let v = h.alloc_cell(Value::int(42)).unwrap();
        f.write(&mut h, RangePosition { range: r, index: 0 }, v)
            .unwrap();
        h.begin_collection().unwrap();
        let budget = GcBudget {
            units: h.root_slots() + 1,
            edges: 0,
            ..GcBudget::default()
        };
        h.gc_step_with_ranges(budget, f.roots()).unwrap(); // single slots + range sentinel
        let slot = index(r.key);
        let w = f.mappings.as_mut().unwrap().data[slot].take().unwrap();
        let used = h.budget_usage().charged_bytes();
        assert_eq!(
            h.gc_step_with_ranges(GcBudget::default(), f.roots())
                .unwrap_err(),
            HeapError::InvalidRootRange
        );
        assert_eq!(h.phase(), GcPhase::Mark);
        assert_eq!(f.close(&mut h).unwrap_err(), HeapError::InvalidRootRange);
        assert_eq!(h.stats().root_ranges, 1);
        assert_eq!(h.budget_usage().charged_bytes(), used);
        f.mappings.as_mut().unwrap().data[slot] = Some(w);
        h.ranges.get_mut(r.key).unwrap().len = 2;
        assert_eq!(
            h.gc_step_with_ranges(GcBudget::default(), f.roots())
                .unwrap_err(),
            HeapError::RootRangeLength {
                expected: 2,
                actual: 1
            }
        );
        assert_eq!(
            f.close(&mut h).unwrap_err(),
            HeapError::RootRangeLength {
                expected: 2,
                actual: 1
            }
        );
        assert_eq!(h.phase(), GcPhase::Mark);
        h.ranges.get_mut(r.key).unwrap().len = 1;
        while h.phase() != GcPhase::Idle {
            h.gc_step_with_ranges(GcBudget::default(), f.roots())
                .unwrap();
        }
        assert_eq!(h.get_field(v, 0).unwrap().as_int(), Some(42));
        f.close(&mut h).unwrap();
        assert_eq!(h.stats().root_ranges, 0);
    }
    #[test]
    fn range_generation_retirement_never_revalidates_old_tokens_or_refunds_slots() {
        let mut h = Heap::with_retirement(
            SessionEpoch(1),
            HeapLimits {
                max_objects: 2,
                max_roots: 1,
                max_root_ranges: 2,
                ..HeapLimits::default()
            },
            3,
        )
        .unwrap();
        let mut f = FrameSlots::new(
            &mut h,
            FrameSlotsLimits {
                initial_capacity: 4,
                max_capacity: 4,
                max_window_len: 4,
            },
        )
        .unwrap();
        let before = h.stats().charged_bytes;
        let mut old = Vec::new();
        for i in 0..4 {
            let r = f
                .open(&mut h, RootOwner(1), RootKind::TaskFrame, 1)
                .unwrap();
            f.write(&mut h, RangePosition { range: r, index: 0 }, Value::int(i))
                .unwrap();
            old.push(r);
            let mut wrong_epoch = r;
            wrong_epoch.epoch = SessionEpoch(2);
            assert_eq!(
                f.read(RangePosition {
                    range: wrong_epoch,
                    index: 0
                })
                .unwrap_err(),
                HeapError::WrongEpoch
            );
            f.release(&mut h, r).unwrap();
            for &t in &old {
                assert_eq!(
                    f.read(RangePosition { range: t, index: 0 }).unwrap_err(),
                    HeapError::InvalidRootRange
                );
            }
        }
        assert_eq!(h.stats().retired_root_ranges, 2);
        assert_eq!(h.stats().charged_bytes, before);
        assert_eq!(
            f.open(&mut h, RootOwner(1), RootKind::TaskFrame, 1)
                .unwrap_err(),
            HeapError::RootRangeCapacity
        );
        f.close(&mut h).unwrap();
        drop(f);
        let mut f = FrameSlots::new(
            &mut h,
            FrameSlotsLimits {
                initial_capacity: 4,
                max_capacity: 4,
                max_window_len: 4,
            },
        )
        .unwrap();
        assert_eq!(
            f.open(&mut h, RootOwner(1), RootKind::TaskFrame, 1)
                .unwrap_err(),
            HeapError::RootRangeCapacity
        );
        assert_eq!(h.stats().retired_root_ranges, 2);
    }
}
