//! Submitted effect pixels replacing proved source lifetimes of one preparation.
use super::{
    GpuError,
    effect_result::{Grid, Token},
    scissor::Scissor,
    target_plan::Requirements,
    target_storage::Event,
};
use reel_ui::{frame::FrameDrawing, kurbo::Affine};
use std::ops::Range;

/// A source raster in one occurrence of an immutable drawing.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Raster {
    transform: [u64; 6],
    size: [u32; 2],
    storage: [u32; 2],
    region: [u32; 4],
}
impl Raster {
    pub(crate) fn new(
        transform: Affine,
        size: [u32; 2],
        storage: [u32; 2],
        region: Scissor,
    ) -> Self {
        Self {
            transform: transform.as_coeffs().map(f64::to_bits),
            size,
            storage,
            region: [region.x, region.y, region.width, region.height],
        }
    }
}

/// Command indices belong to their drawing; raster identity also distinguishes
/// repeated consumers of that drawing under different frame contexts.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Key {
    drawing: usize,
    push: usize,
    raster: Raster,
}
impl Key {
    pub(crate) fn new(drawing: &FrameDrawing, push: usize, raster: Raster) -> Self {
        Self {
            drawing: std::ptr::from_ref(drawing) as usize,
            push,
            raster,
        }
    }
}

/// The root defers its source until `input` reads these existing pixels. The
/// input's own effect and its ancestors' materialized outputs still execute.
#[derive(Clone)]
pub(crate) struct Entry {
    pub root: Key,
    pub input: Key,
    pub resume: usize,
    pub commands: Range<usize>,
    pub token: Token,
    pub grid: Grid,
}

/// Backend-local physical owner identifier and its complete allocation cost.
/// Multiple views of the same atlas report the same owner.
#[derive(Clone, Copy)]
pub(crate) struct Hit {
    pub token: Token,
    pub owner: usize,
    pub bytes: u64,
}

pub(super) struct Candidate {
    pub entry: Entry,
    pub owner: Hit,
    /// Initial source acquisition and every producer before the input consumer.
    pub events: Range<usize>,
    /// The consumer's release of that input; its output acquisition is retained.
    pub release: usize,
}

/// Only producer events and their input retirement are removed. Deferred
/// siblings may produce between this prefix and its later retirement.
pub(super) fn exclusions_overlap(
    first: &Range<usize>,
    first_release: usize,
    second: &Range<usize>,
    second_release: usize,
) -> bool {
    first.start < second.end && second.start < first.end
        || first.contains(&second_release)
        || second.contains(&first_release)
        || first_release == second_release
}

#[derive(Default, Clone, Copy)]
#[cfg_attr(
    not(feature = "profiling"),
    allow(
        dead_code,
        reason = "preparation diagnostics are consumed by profiling builds"
    )
)]
pub(crate) struct Stats {
    pub scans: u64,
    pub candidates: u64,
    pub events: u64,
    pub recounts: u64,
    pub rejected_owners: u64,
    pub scan_ns: u64,
    pub recount_ns: u64,
}

/// Frozen metadata; all backend handles and in-flight pixel guards stay outside.
#[derive(Default)]
pub(crate) struct Plan {
    pub(super) entries: Vec<Entry>,
    pub(super) owners: Vec<usize>,
    pub(super) requirements: Requirements,
    pub(super) bytes: u64,
    pub(super) pinned_bytes: u64,
    pub(super) fixed_bytes: u64,
    pub(super) events: Vec<Event>,
    #[cfg(any(feature = "gxm", test))]
    pub(super) macrotiles: Vec<super::target_plan::macrotile::Candidate>,
    #[cfg_attr(
        not(feature = "profiling"),
        allow(
            dead_code,
            reason = "preparation diagnostics are consumed by profiling builds"
        )
    )]
    pub stats: Stats,
}
impl Plan {
    pub(super) fn diagnosed(self) -> Self {
        #[cfg(test)]
        testing::observe(&self);
        #[cfg(feature = "profiling")]
        crate::profiling::record_effect_input_plan(
            self.stats,
            self.entries.len() as u64,
            self.pinned_bytes,
        );
        self
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub(crate) fn root(&self, key: &Key) -> Option<usize> {
        self.entries
            .binary_search_by_key(key, |entry| entry.root)
            .ok()
    }
    pub(crate) fn entry(&self, index: usize) -> &Entry {
        &self.entries[index]
    }
    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }
    pub(crate) fn owners(&self) -> &[usize] {
        &self.owners
    }
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
    pub(crate) fn pinned_bytes(&self) -> u64 {
        self.pinned_bytes
    }
    pub(crate) fn requirements(&self) -> &Requirements {
        &self.requirements
    }
}

/// Select disjoint producer prefixes, then remove whole physical-owner groups
/// until their scratch recount and distinct pixel allocations fit together.
/// The callback only counts frozen events; it never resolves a drawing again.
pub(super) fn choose(
    candidates: &[Candidate],
    limit: u64,
    stats: &mut Stats,
    mut measure: impl FnMut(&[usize]) -> Result<u64, GpuError>,
    mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
) -> Result<Vec<usize>, GpuError> {
    let count = candidates.len();
    reserve((count * size_of::<usize>() * 4) as u64)?;
    let mut order: Vec<_> = (0..count).collect();
    order.sort_unstable_by_key(|&i| {
        (
            candidates[i].events.start,
            std::cmp::Reverse(candidates[i].events.end),
        )
    });
    let mut rejected = Vec::with_capacity(count);
    let mut chosen: Vec<usize> = Vec::with_capacity(count);
    let mut owners = Vec::with_capacity(count);
    loop {
        chosen.clear();
        owners.clear();
        let mut pinned = 0u64;
        for &index in &order {
            let candidate = &candidates[index];
            if rejected.contains(&candidate.owner.owner)
                || chosen.iter().any(|&old| {
                    let old = &candidates[old];
                    exclusions_overlap(
                        &old.events,
                        old.release,
                        &candidate.events,
                        candidate.release,
                    )
                })
            {
                continue;
            }
            chosen.push(index);
            if !owners.contains(&candidate.owner.owner) {
                owners.push(candidate.owner.owner);
                pinned = pinned
                    .checked_add(candidate.owner.bytes)
                    .ok_or(GpuError::InvalidSize)?;
            }
        }
        stats.recounts += 1;
        let scratch = measure(&chosen)?;
        if scratch.checked_add(pinned).ok_or(GpuError::InvalidSize)? <= limit {
            return Ok(chosen);
        }
        // Compare covered commands per physical byte. Shared views contribute
        // coverage together while their complete owner is charged only once.
        let mut worst: Option<(usize, u64, u64)> = None;
        for owner in owners.iter().copied() {
            let mut covered = 0u64;
            let mut bytes = 0u64;
            for &index in &chosen {
                let candidate = &candidates[index];
                if candidate.owner.owner == owner {
                    covered += candidate.entry.commands.len() as u64;
                    bytes = candidate.owner.bytes;
                }
            }
            if worst.is_none_or(|(_, old_coverage, old_bytes)| {
                u128::from(covered) * u128::from(old_bytes)
                    < u128::from(old_coverage) * u128::from(bytes)
            }) {
                worst = Some((owner, covered, bytes));
            }
        }
        let Some((owner, _, _)) = worst else {
            // A cold scratch plan may still exceed the limit before its batch
            // or raster fallback. No input choice admitted this frozen attempt.
            return Ok(chosen);
        };
        rejected.push(owner);
        stats.rejected_owners += 1;
    }
}

/// Sorted event exclusions include a disjoint producer span and one later read
/// retirement per chosen input. Merge them for the common high-water recount.
pub(super) fn exclusions(candidates: &[Candidate], chosen: &[usize], out: &mut Vec<Range<usize>>) {
    out.clear();
    for &index in chosen {
        let candidate = &candidates[index];
        out.push(candidate.events.clone());
        out.push(candidate.release..candidate.release + 1);
    }
    out.sort_unstable_by_key(|range| range.start);
    let mut written = 0;
    for read in 0..out.len() {
        let range = out[read].clone();
        if written != 0 && out[written - 1].end >= range.start {
            out[written - 1].end = out[written - 1].end.max(range.end);
        } else {
            out[written] = range;
            written += 1;
        }
    }
    out.truncate(written);
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    pub(crate) struct Sample {
        pub inputs: usize,
        pub bytes: u64,
        pub pins: u64,
        pub stats: Stats,
    }
    std::thread_local! {
        static TRACE: std::cell::RefCell<Option<Vec<Sample>>> = const { std::cell::RefCell::new(None) };
    }
    pub(super) fn observe(plan: &Plan) {
        TRACE.with_borrow_mut(|trace| {
            if let Some(trace) = trace {
                trace.push(Sample {
                    inputs: plan.entries.len(),
                    bytes: plan.bytes,
                    pins: plan.pinned_bytes,
                    stats: plan.stats,
                });
            }
        });
    }
    pub(crate) fn collect<T>(run: impl FnOnce() -> T) -> (T, Vec<Sample>) {
        TRACE.with_borrow_mut(|trace| {
            assert!(trace.is_none());
            *trace = Some(Vec::new());
        });
        let result = run();
        (result, TRACE.with_borrow_mut(|trace| trace.take().unwrap()))
    }
}
