use crate::payload::{Payload, VALUE_CHUNK_BYTES};
use crate::slots::RangeKey;
use crate::{Heap, HeapError, RootRangeToken, RootRanges};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum GcPhase {
    #[default]
    Idle,
    Mark,
    Weak,
    Sweep,
}

/// Independent quotas; bytes include the charged payload and chunk metadata.
/// A byte quota smaller than one chunk cannot free that chunk and reports blocked.
#[derive(Clone, Copy, Debug)]
pub struct GcBudget {
    pub roots: u32,
    pub edges: u32,
    pub weak: u32,
    pub objects: u32,
    pub release_edges: u32,
    pub bytes: u64,
    pub units: u32,
}
impl Default for GcBudget {
    fn default() -> Self {
        Self {
            roots: 256,
            edges: 256,
            weak: 256,
            objects: 256,
            release_edges: 256,
            bytes: 16_384,
            units: 256,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GcProgress {
    pub phase: GcPhase,
    pub roots: u32,
    pub single_root_slots: u32,
    pub range_descriptors: u32,
    pub range_holes: u32,
    pub range_values: u32,
    pub edges: u32,
    pub weak: u32,
    pub objects: u32,
    pub release_edges: u32,
    pub bytes: u64,
    pub units: u32,
    pub invalidated_objects: u32,
    pub released_objects: u32,
    pub blocked: bool,
    pub cycle_complete: bool,
}

#[derive(Default)]
pub(crate) struct Collector {
    pub phase: GcPhase,
    pub cycle: u64,
    pub units: u64,
    pub allocated_objects: u64,
    pub allocated_bytes: u64,
    live_objects: u32,
    live_payload: u64,
    requested: bool,
    pub root_cursor: usize,
    range_slot: usize,
    range_key: Option<RangeKey>,
    range_offset: u32,
    weak_cursor: usize,
    cutoff_len: usize,
    sweep_remaining: usize,
}
impl Collector {
    pub fn cutoff(&self) -> bool {
        matches!(self.phase, GcPhase::Weak | GcPhase::Sweep)
    }
}

impl Heap {
    /// Whether allocation since the last cycle warrants another incremental cycle.
    /// This is a host scheduling hint; explicit collection always starts immediately.
    pub fn collection_due(&self) -> bool {
        let available_objects = self
            .limits
            .max_objects
            .saturating_sub(self.objects.occupied() as u32);
        let available_bytes = self.usage.remaining_bytes();
        let object_threshold = u64::from((self.gc.live_objects / 2).max(256))
            .min(u64::from(available_objects / 2))
            .min(available_bytes / (2 * std::mem::size_of::<crate::heap::Object>() as u64))
            .max(1);
        let byte_threshold = (self.gc.live_payload / 2)
            .max(64 * 1024)
            .min(available_bytes / 2)
            .max(1);
        self.gc.phase == GcPhase::Idle
            && (self.gc.requested
                || self.gc.allocated_objects >= object_threshold
                || self.gc.allocated_bytes >= byte_threshold)
    }

    /// Request a cycle at the next host safe point, including after a running cycle.
    pub fn request_collection(&mut self) {
        self.gc.requested = true;
    }

    pub fn begin_collection(&mut self) -> Result<(), HeapError> {
        if self.gc.phase != GcPhase::Idle {
            return Err(HeapError::CollectionInProgress);
        }
        self.gc.cycle = self
            .gc
            .cycle
            .checked_add(1)
            .ok_or(HeapError::CycleExhausted)?;
        self.gc.allocated_objects = 0;
        self.gc.allocated_bytes = 0;
        self.gc.requested = false;
        self.gc.root_cursor = 0;
        self.gc.range_slot = 0;
        self.gc.range_key = None;
        self.gc.range_offset = 0;
        self.gc.phase = GcPhase::Mark;
        Ok(())
    }

    /// Run only at the host's single-threaded safe points; never calls script/native code.
    /// Each operation is an edge/slot/chunk, with a total units cap between clock checks.
    pub fn gc_step(&mut self, budget: GcBudget) -> Result<GcProgress, HeapError> {
        self.gc_step_impl(budget, None)
    }

    pub fn gc_step_with_ranges(
        &mut self,
        budget: GcBudget,
        roots: RootRanges<'_>,
    ) -> Result<GcProgress, HeapError> {
        self.gc_step_impl(budget, Some(roots))
    }

    fn gc_step_impl(
        &mut self,
        budget: GcBudget,
        roots: Option<RootRanges<'_>>,
    ) -> Result<GcProgress, HeapError> {
        // Refuse before even Idle/zero-unit calls, or a Weak/Sweep phase transition.
        if let Some(provider) = roots {
            provider.check_heap(self)?;
        } else if self.ranges.occupied() != 0 {
            return Err(HeapError::MissingRootProvider);
        }
        let mut p = GcProgress::default();
        while p.units < budget.units {
            match self.gc.phase {
                GcPhase::Idle => break,
                GcPhase::Mark => {
                    if self.gc.root_cursor < self.root_scan_len {
                        if p.roots == budget.roots {
                            p.blocked = true;
                            break;
                        }
                        self.scan_root(self.gc.root_cursor);
                        self.gc.root_cursor += 1;
                        p.roots += 1;
                        p.single_root_slots += 1;
                        p.units += 1;
                    } else if self.gc.range_slot < self.range_scan_len {
                        if p.roots == budget.roots {
                            p.blocked = true;
                            break;
                        }
                        self.scan_range_unit(roots, &mut p)?;
                        p.roots += 1;
                        p.units += 1;
                    } else if let Some(&key) = self.gray.front() {
                        // Empty-object completion is also a charged work unit.
                        if p.edges == budget.edges {
                            p.blocked = true;
                            break;
                        }
                        let obj = self
                            .objects
                            .get_mut(key)
                            .expect("gray entry owns a live slot");
                        if !obj.metadata_scanned
                            && !obj.model_metadata.same_bits(crate::Value::NULL)
                        {
                            obj.metadata_scanned = true;
                            let metadata = obj.model_metadata;
                            self.shade(metadata);
                            p.edges += 1;
                        } else if !obj.reactive_scanned
                            && !obj.reactive_subscribers.same_bits(crate::Value::NULL)
                        {
                            obj.reactive_scanned = true;
                            let metadata = obj.reactive_subscribers;
                            self.shade(metadata);
                            p.edges += 1;
                        } else if let Some(value) = obj.payload.edge(obj.scan) {
                            obj.metadata_scanned = true;
                            obj.scan += 1;
                            self.shade(value);
                            p.edges += 1;
                        } else {
                            obj.queued = false;
                            self.gray.pop_front();
                        }
                        p.units += 1;
                    } else {
                        // This safe point is the single weak-liveness cutoff.
                        self.gc.cutoff_len = self.allocations.len();
                        self.gc.weak_cursor = 0;
                        self.gc.sweep_remaining = self.gc.cutoff_len;
                        self.gc.phase = GcPhase::Weak;
                    }
                }
                GcPhase::Weak => {
                    if self.gc.weak_cursor == self.gc.cutoff_len {
                        self.gc.phase = GcPhase::Sweep;
                        continue;
                    }
                    if p.weak == budget.weak {
                        p.blocked = true;
                        break;
                    }
                    // Stable indexed pass: don't rotate the allocation queue here.
                    // New allocations append after cutoff_len and wait for the next cycle.
                    let key = self.allocations[self.gc.weak_cursor];
                    let obj = self
                        .objects
                        .get(key)
                        .expect("allocation record corresponds to slot");
                    let target = obj.weak;
                    if target.is_some_and(|h| !self.weak_confirmed(h)) {
                        self.objects.get_mut(key).unwrap().weak = None;
                    }
                    self.gc.weak_cursor += 1;
                    p.weak += 1;
                    p.units += 1;
                }
                GcPhase::Sweep => {
                    // Cleanup may progress independently of this call's sweep quota.
                    if self.release_step(&budget, &mut p) {
                        continue;
                    }
                    if self.gc.sweep_remaining != 0 {
                        if p.objects == budget.objects {
                            p.blocked = true;
                            break;
                        }
                        let key = self
                            .allocations
                            .pop_front()
                            .expect("remaining allocation record");
                        let obj = self
                            .objects
                            .get_mut(key)
                            .expect("allocation record corresponds to slot");
                        if obj.mark == self.gc.cycle {
                            self.allocations.push_back(key);
                        } else {
                            // Reject lookups immediately; payload remains fully charged.
                            obj.dying = true;
                            self.pending_bytes += obj.charged;
                            self.peak_pending_bytes =
                                self.peak_pending_bytes.max(self.pending_bytes);
                            self.release.push_back(key);
                            p.invalidated_objects += 1;
                        }
                        self.gc.sweep_remaining -= 1;
                        p.objects += 1;
                        p.units += 1;
                    } else if self.release.is_empty() {
                        self.gc.live_objects = self.objects.occupied() as u32;
                        self.gc.live_payload = self.stats().payload_bytes;
                        self.gc.phase = GcPhase::Idle;
                        p.cycle_complete = true;
                    } else {
                        p.blocked = true;
                        break;
                    }
                }
            }
        }
        p.phase = self.gc.phase;
        self.gc.units = self.gc.units.saturating_add(u64::from(p.units));
        Ok(p)
    }

    fn scan_range_unit(
        &mut self,
        roots: Option<RootRanges<'_>>,
        p: &mut GcProgress,
    ) -> Result<(), HeapError> {
        let Some(key) = self.range_index[self.gc.range_slot] else {
            self.gc.range_key = None;
            self.gc.range_offset = 0;
            self.gc.range_slot += 1;
            p.range_holes += 1;
            return Ok(());
        };
        let token = RootRangeToken::new(self, key);
        let d = self.range_descriptor(token)?;
        let values = roots
            .ok_or(HeapError::MissingRootProvider)?
            .resolve(token)?;
        if values.len() != d.len as usize {
            return Err(HeapError::RootRangeLength {
                expected: d.len,
                actual: values.len() as u32,
            });
        }
        if self.gc.range_key != Some(key) {
            // A different full generation never inherits the previous element cursor.
            self.gc.range_key = Some(key);
            self.gc.range_offset = 0;
            p.range_descriptors += 1;
            if d.len == 0 {
                self.gc.range_slot += 1;
                self.gc.range_key = None;
            }
        } else {
            let value = values[self.gc.range_offset as usize];
            self.shade(value);
            self.gc.range_offset += 1;
            p.range_values += 1;
            if self.gc.range_offset == d.len {
                self.gc.range_slot += 1;
                self.gc.range_key = None;
                self.gc.range_offset = 0;
            }
        }
        Ok(())
    }

    fn release_step(&mut self, budget: &GcBudget, p: &mut GcProgress) -> bool {
        let Some(&key) = self.release.front() else {
            return false;
        };
        let obj = self
            .objects
            .get_mut(key)
            .expect("release record owns a dying slot");
        let bytes = match &mut obj.payload {
            Payload::Fault(value) if value.release_cursor < 3 => {
                if p.release_edges == budget.release_edges {
                    return false;
                }
                value.edges[value.release_cursor as usize] = crate::Value::NULL;
                value.release_cursor += 1;
                p.release_edges += 1;
                p.units += 1;
                return true;
            }
            Payload::Cell(value) if value.value.is_some() => {
                if p.release_edges == budget.release_edges {
                    return false;
                }
                value.value = None;
                p.release_edges += 1;
                p.units += 1;
                return true;
            }
            Payload::Dependencies(d) if !d.index.is_empty() => {
                d.index.pop_first();
                if d.index.is_empty() {
                    drop(std::mem::take(&mut d.index));
                }
                p.units += 1;
                return true;
            }
            Payload::Dependencies(d) if d.index_reserved != 0 => {
                let released = d
                    .index_reserved
                    .min(VALUE_CHUNK_BYTES)
                    .min(budget.bytes.saturating_sub(p.bytes));
                if released == 0 {
                    return false;
                }
                d.index_reserved -= released;
                released
            }
            Payload::Dependencies(d) if !d.values.empty() => {
                let values = &mut d.values;
                let remaining = values.first_chunk_len() - values.release_cursor;
                if remaining != 0 {
                    if p.release_edges == budget.release_edges {
                        return false;
                    }
                    values.clear_release_edge();
                    p.release_edges += 1;
                    p.units += 1;
                    return true;
                }
                let released = values.release_bytes();
                if budget.bytes.saturating_sub(p.bytes) < released {
                    return false;
                }
                values.drop_first_chunk();
                released
            }
            Payload::Dependencies(_) => {
                let released = crate::dependencies::BASE_BYTES;
                if budget.bytes.saturating_sub(p.bytes) < released {
                    return false;
                }
                obj.payload = Payload::Empty;
                released
            }
            Payload::Values(values) | Payload::Table(values) if !values.empty() => {
                let remaining = values.first_chunk_len() - values.release_cursor;
                if remaining != 0 {
                    if p.release_edges == budget.release_edges {
                        return false;
                    }
                    // Values contain integer handles: clearing an edge cannot recurse.
                    values.clear_release_edge();
                    p.release_edges += 1;
                    p.units += 1;
                    return true;
                }
                let released = values.release_bytes();
                if budget.bytes.saturating_sub(p.bytes) < released {
                    return false;
                }
                values.drop_first_chunk();
                released
            }
            Payload::Bytes(bytes) if !bytes.empty() => {
                let released = bytes.release_bytes();
                if budget.bytes.saturating_sub(p.bytes) < released {
                    return false;
                }
                bytes.drop_first_chunk();
                released
            }
            _ => {
                // Empty payload before remove guarantees no recursive/unbounded drop.
                assert_eq!(obj.charged, 0);
                self.remove_object(key);
                self.release.pop_front();
                p.released_objects += 1;
                p.units += 1;
                return true;
            }
        };
        obj.charged -= bytes;
        self.pending_bytes -= bytes;
        self.freed(bytes);
        p.bytes += bytes;
        p.units += 1;
        true
    }
}
