//! Native target owners using the common physical-demand and recency policy.
use super::resources::Target;
use crate::common::{target_cache, target_plan::Requirements};
use reel_core::budget::{BudgetPool, BudgetTicket};
use smallvec::SmallVec;
use std::rc::Rc;

#[derive(Clone)]
pub(super) struct Macrotile {
    pub cells: SmallVec<[Target; 4]>,
    _metadata: Option<Rc<BudgetTicket>>,
}
impl Macrotile {
    pub(super) fn new(cells: SmallVec<[Target; 4]>) -> Self {
        Self {
            cells,
            _metadata: None,
        }
    }
    fn cache_only(&self) -> bool {
        self.cells[0].cache_only_owners(self.cells.len())
    }
    fn size(&self) -> [u32; 2] {
        self.cells[0].macrotile_extent().expect("macro owner")
    }
}

#[derive(Default)]
pub(super) struct Cache {
    size: Option<[u32; 2]>,
    pub(super) groups: crate::common::scopes::Cache,
    owners: target_cache::Cache<Target>,
    macrotiles: target_cache::Cache<Macrotile>,
}
impl Cache {
    pub(super) fn capacities(
        &self,
        storage: super::native::TargetStorage,
        used: &[Target],
        reserve: impl FnMut(u64) -> Result<(), crate::common::GpuError>,
    ) -> Result<Requirements, crate::common::GpuError> {
        self.owners.capacities(
            |target| {
                target.view.target_storage() == Some(storage)
                    && !target.is_effect_pinned()
                    && !used.iter().any(|held| held.same_allocation(target))
            },
            reserve,
        )
    }
    pub fn output_size(&self) -> Option<[u32; 2]> {
        self.size
    }
    pub fn configure(&mut self, size: [u32; 2]) {
        if self.size != Some(size) {
            self.clear();
            self.size = Some(size);
        }
    }
    pub fn get(
        &self,
        size: [u32; 2],
        storage: super::native::TargetStorage,
        used: &[Target],
    ) -> Option<Target> {
        let result = self
            .owners
            .get_eligible(size, 0, |target| {
                let reason = if target.view.target_storage() != Some(storage) {
                    Some("ordinary-reject-storage")
                } else if target.is_effect_pinned() {
                    Some("ordinary-reject-pinned")
                } else if used.iter().any(|held| held.same_allocation(target)) {
                    Some("ordinary-reject-in-use")
                } else {
                    None
                };
                #[cfg(feature = "profiling")]
                if let Some(reason) = reason {
                    trace_rejected(target, reason, 1);
                }
                reason.is_none()
            })
            .cloned();
        #[cfg(feature = "profiling")]
        if let Some(target) = &result {
            target.trace_event(crate::profiling::TraceKind::TargetCache, "ordinary-hit");
        } else {
            trace_miss("ordinary-miss", size, 1);
        }
        result
    }
    /// Rebind an equal-byte linear owner without changing its physical charge.
    /// Future exact-size requirements retain their complete scratch multiplicity.
    pub(super) fn reshape(
        &mut self,
        gpu: &super::ownership::Device,
        size: [u32; 2],
        format: super::native::TargetFormat,
        storage: super::native::TargetStorage,
        used: &[Target],
        required: &Requirements,
    ) -> Result<Option<Target>, crate::common::GpuError> {
        if storage != super::native::TargetStorage::Linear {
            return Ok(None);
        }
        let bytes = storage.layout(size, format)?.texture_bytes;
        let format = match format {
            super::native::TargetFormat::Rgba8 => super::native::TextureFormat::Rgba8,
            super::native::TargetFormat::Coverage8 => super::native::TextureFormat::Coverage8,
        };
        let mut previous = None;
        let mut index = 0;
        self.owners.map_eligible(
            size,
            |target| {
                if target.view.target_storage() != Some(storage)
                    || target.view.format() != format
                    || target.is_effect_pinned()
                {
                    return false;
                }
                let old = target.view.size();
                if previous == Some(old) {
                    index += 1;
                } else {
                    previous = Some(old);
                    index = 0;
                }
                old != size
                    && required.count(old) <= index
                    && target.view.allocated_bytes() as u64 == bytes
                    && !used.iter().any(|held| held.same_allocation(target))
            },
            |target| target.reshaped(gpu, size),
        )
    }
    pub fn insert(&mut self, target: &Target, metadata: &BudgetPool, limit: u64) {
        // Optional cache bookkeeping must not reject a resident target. Its work
        // owner remains valid when no extra cache slot fits the shared budget.
        let _ = self.owners.reserve(1, metadata, limit);
        let stored = self.owners.insert(target.view.size(), target.clone());
        #[cfg(feature = "profiling")]
        {
            let mut data = target.trace_data();
            data.label = "ordinary-insert";
            data.status = if stored {
                crate::profiling::TraceStatus::Ok
            } else {
                crate::profiling::TraceStatus::Skipped
            };
            crate::profiling::record_trace(crate::profiling::TraceKind::TargetCache, data);
        }
        #[cfg(not(feature = "profiling"))]
        let _ = stored;
    }
    pub fn macrotile(
        &self,
        extent: [u32; 2],
        sizes: &[[u32; 2]],
        mut in_use: impl FnMut(&Target) -> bool,
    ) -> Option<Macrotile> {
        #[cfg(feature = "profiling")]
        trace_request("macro-request", extent, sizes.len() as u64);
        let result = self
            .macrotiles
            .get_eligible(extent, 0, |set| {
                // Previous submissions read before this frame writes on the same
                // graphics queue. Exclude whole owners held by this preparation
                // or immutable effect results, including pins on another cell.
                let reason = if set.cells[0].is_effect_pinned() {
                    Some("macro-reject-pinned")
                } else if in_use(&set.cells[0]) {
                    Some("macro-reject-in-use")
                } else if set.cells.len() != sizes.len() {
                    Some("macro-reject-cell-count")
                } else if !set
                    .cells
                    .iter()
                    .zip(sizes)
                    .all(|(cell, &size)| cell.view.size() == size)
                {
                    Some("macro-reject-cell-size")
                } else {
                    None
                };
                #[cfg(feature = "profiling")]
                if let Some(reason) = reason {
                    trace_rejected(&set.cells[0], reason, set.cells.len() as u64);
                }
                reason.is_none()
            })
            .cloned();
        #[cfg(feature = "profiling")]
        if let Some(set) = &result {
            let mut data = set.cells[0].trace_data();
            data.label = "macro-hit";
            data.status = crate::profiling::TraceStatus::Ok;
            data.count = set.cells.len() as u64;
            crate::profiling::record_trace(crate::profiling::TraceKind::TargetCache, data);
        } else {
            trace_miss("macro-miss", extent, sizes.len() as u64);
        }
        result
    }
    pub fn insert_macrotile(&mut self, set: &Macrotile, metadata: &BudgetPool, limit: u64) {
        let _ = self.macrotiles.reserve(1, metadata, limit);
        let storage = if set.cells.spilled() {
            let bytes = (set.cells.capacity() * size_of::<Target>()) as u64;
            let Ok(ticket) = metadata.reserve(bytes) else {
                #[cfg(feature = "profiling")]
                trace_rejected(
                    &set.cells[0],
                    "macro-metadata-denied",
                    set.cells.len() as u64,
                );
                return;
            };
            Some(Rc::new(ticket))
        } else {
            None
        };
        let mut retained = set.clone();
        retained._metadata = storage;
        let stored = self.macrotiles.insert(set.size(), retained);
        #[cfg(feature = "profiling")]
        {
            let mut data = set.cells[0].trace_data();
            data.label = "macro-insert";
            data.count = set.cells.len() as u64;
            data.status = if stored {
                crate::profiling::TraceStatus::Ok
            } else {
                crate::profiling::TraceStatus::Skipped
            };
            crate::profiling::record_trace(crate::profiling::TraceKind::TargetCache, data);
        }
        #[cfg(not(feature = "profiling"))]
        let _ = stored;
    }
    pub fn reclaim(&mut self, required: &Requirements, recent: bool) -> bool {
        self.owners.reclaim_eligible_observed(
            required,
            recent,
            |target| !target.is_effect_pinned(),
            Target::cache_only,
            |target, age, uses| {
                #[cfg(feature = "profiling")]
                trace_eviction(target, "ordinary-pressure-evict", age, uses);
                #[cfg(not(feature = "profiling"))]
                let _ = (target, age, uses);
            },
        ) || self.macrotiles.reclaim_eligible_observed(
            &Requirements::default(),
            recent,
            |_| true,
            Macrotile::cache_only,
            |set, age, uses| {
                #[cfg(feature = "profiling")]
                trace_eviction(&set.cells[0], "macro-pressure-evict", age, uses);
                #[cfg(not(feature = "profiling"))]
                let _ = (set, age, uses);
            },
        )
    }
    /// Required ordinary or macrotile storage uses the same pressure policy.
    pub(super) fn reclaim_for_target(
        &mut self,
        required: &Requirements,
        results: &crate::common::effect_result::Cache<Target>,
        recent: bool,
    ) -> bool {
        self.reclaim(required, recent)
            || results.reclaim_output()
            || recent && self.reclaim_unclaimed()
    }
    /// A future scratch size is a reuse preference, not a live GPU owner. Its
    /// idle allocation may be rebuilt after a larger physical block is acquired.
    pub fn reclaim_unclaimed(&mut self) -> bool {
        self.reclaim(&Requirements::default(), true)
    }
    pub fn submitted(&mut self, used: &[Target]) {
        self.owners.submitted();
        self.macrotiles.submitted();
        for target in used {
            if let Some(extent) = target.macrotile_extent() {
                self.macrotiles
                    .mark_used(extent, |cached| cached.cells[0].same_allocation(target));
            } else {
                self.owners
                    .mark_used(target.view.size(), |cached| cached.same_allocation(target));
            }
        }
    }
    pub fn prune_cold(&mut self) {
        self.owners.prune_cold(|target| {
            let unique = target.cache_only();
            #[cfg(feature = "profiling")]
            if unique {
                target.trace_event(
                    crate::profiling::TraceKind::TargetCache,
                    "ordinary-cold-evict",
                );
            }
            unique
        });
        self.macrotiles.prune_cold(|set| {
            let unique = set.cache_only();
            #[cfg(feature = "profiling")]
            if unique {
                set.cells[0]
                    .trace_event(crate::profiling::TraceKind::TargetCache, "macro-cold-evict");
            }
            unique
        });
    }
    pub fn release_idle(&mut self) {
        self.owners.release_idle(|target| {
            let unique = target.cache_only();
            #[cfg(feature = "profiling")]
            if unique {
                target.trace_event(
                    crate::profiling::TraceKind::TargetCache,
                    "ordinary-idle-evict",
                );
            }
            unique
        });
        self.macrotiles.release_idle(|set| {
            let unique = set.cache_only();
            #[cfg(feature = "profiling")]
            if unique {
                set.cells[0]
                    .trace_event(crate::profiling::TraceKind::TargetCache, "macro-idle-evict");
            }
            unique
        });
    }
    pub fn clear(&mut self) {
        #[cfg(feature = "profiling")]
        {
            self.owners.observe(|target, age, uses| {
                trace_eviction(target, "ordinary-cache-clear", age, uses);
            });
            self.macrotiles.observe(|set, age, uses| {
                trace_eviction(&set.cells[0], "macro-cache-clear", age, uses);
            });
        }
        *self = Self::default();
    }
}

#[cfg(feature = "profiling")]
fn trace_request(label: &'static str, size: [u32; 2], count: u64) {
    crate::profiling::record_trace(
        crate::profiling::TraceKind::TargetCache,
        crate::profiling::TraceData {
            label,
            storage: size,
            count,
            known: crate::profiling::KNOWN_STORAGE,
            ..Default::default()
        },
    );
}

#[cfg(feature = "profiling")]
fn trace_miss(label: &'static str, size: [u32; 2], count: u64) {
    crate::profiling::record_trace(
        crate::profiling::TraceKind::TargetCache,
        crate::profiling::TraceData {
            label,
            status: crate::profiling::TraceStatus::Skipped,
            storage: size,
            count,
            known: crate::profiling::KNOWN_STORAGE,
            ..Default::default()
        },
    );
}

#[cfg(feature = "profiling")]
fn trace_rejected(target: &Target, label: &'static str, count: u64) {
    let mut data = target.trace_data();
    data.label = label;
    data.status = crate::profiling::TraceStatus::Skipped;
    data.count = count;
    crate::profiling::record_trace(crate::profiling::TraceKind::TargetCache, data);
}

#[cfg(feature = "profiling")]
fn trace_eviction(target: &Target, label: &'static str, age: u8, uses: u8) {
    let mut data = target.trace_data();
    data.label = label;
    data.status = crate::profiling::TraceStatus::Ok;
    data.count = u64::from(uses);
    // Source and target dimensions remain unchanged; age is a separate row so
    // the fixed payload does not overload geometry, bytes or native error codes.
    crate::profiling::record_trace(crate::profiling::TraceKind::TargetCache, data);
    data.label = "evicted-age-submissions";
    data.count = u64::from(age);
    crate::profiling::record_trace(crate::profiling::TraceKind::TargetCache, data);
}
