//! One source proof scan, followed by physical-owner selection on frozen events.
use super::*;
use crate::common::{
    effect_inputs as selected,
    effect_result::{Grid, chain},
};
use reel_core::budget::BudgetPool;

#[derive(Clone, Copy)]
pub(super) struct Source {
    pub key: selected::Key,
    pub root: selected::Key,
    pub begin: usize,
}
pub(super) fn ordinary(group: &FrameGroup) -> bool {
    group.effect.is_some()
        && group.mask_effect.is_none()
        && group.clip.is_none()
        && group.mask.is_none()
        && group.image_mask.is_none()
        && group.filters.is_empty()
        && group.invert == 0.
}
pub(super) struct Collector<'a> {
    quality: [u32; 2],
    pool: &'a BudgetPool,
    lookup: &'a mut dyn FnMut(chain::SourceProof<'_>) -> Option<selected::Hit>,
    scans: u64,
}
impl Collector<'_> {
    pub(super) fn candidate(
        &mut self,
        current: &Stream<'_>,
        group: &FrameGroup,
        transform: Affine,
        parent_scissor: Scissor,
        schedule: &offscreen::Plan,
    ) -> Option<selected::Candidate> {
        let parent = current.scopes.last().and_then(|scope| scope.source_owner)?;
        let offset = current.offset?;
        let push = current.next - 1;
        let end = crate::common::effect_result::source_end(current.commands, push)?;
        // Forwarding proves these source and output rasters identical. The
        // parent's source scissor remains its consumer's complete visible input.
        let output = Scissor::group_bounds(group, current.transform, current.size, false)
            .map_or(parent_scissor, |bounds| parent_scissor.intersect(bounds));
        let (source_transform, effect_transform) = Grid::transforms(current.transform, transform);
        let grid = Grid {
            source_transform,
            effect_transform,
            effect_context: None,
            source_size: current.size,
            source_storage: schedule.storage_size(current.size),
            source_scissor: [
                current.scissor.x,
                current.scissor.y,
                current.scissor.width,
                current.scissor.height,
            ],
            output_size: current.size,
            output_storage: schedule.storage_size(current.size),
            scissor: [output.x, output.y, output.width, output.height],
            quality: self.quality,
        };
        let commands = offset + push..offset + end;
        self.scans += 1;
        let proof = chain::source(
            current.drawing,
            commands.clone(),
            grid,
            &current.groups,
            &|size| schedule.storage_size(size),
            self.pool,
        )?;
        let grid = proof.grid();
        let hit = (self.lookup)(proof)?;
        Some(selected::Candidate {
            entry: selected::Entry {
                root: parent.root,
                input: parent.key,
                resume: 0,
                commands,
                token: hit.token,
                grid,
            },
            owner: hit,
            events: parent.begin..0,
            release: usize::MAX,
        })
    }
}

impl Scratch {
    pub(super) fn release_input(
        &mut self,
        scope: &Scope<'_>,
        size: [u32; 2],
        reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<(), GpuError> {
        if let Some(index) = scope.input_candidate {
            self.effect_inputs[index].release = self.events.as_ref().unwrap().len();
        }
        self.release(size, reserve)
    }
}

impl Input<'_> {
    pub(super) fn effect_input_scratch(
        &self,
        record: bool,
        select_macrotiles: bool,
        reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Scratch, GpuError> {
        let inputs = self.schedule.effect_inputs();
        let mut scratch = Scratch::default();
        recount(
            &inputs.events,
            std::iter::empty(),
            |size| self.schedule.storage_size(size),
            &mut scratch,
            reserve,
        )?;
        for (size, count) in self.schedule.coverage_requirements().iter() {
            super::super::scopes::push_stack(
                &mut scratch.coverage,
                Bucket {
                    size,
                    live: 0,
                    peak: count,
                },
                &mut *reserve,
            )?;
        }
        if record {
            reserve(
                (inputs.events.len() * size_of::<super::super::target_storage::Event>()) as u64,
            )?;
            scratch.events = Some(inputs.events.clone());
        }
        #[cfg(any(feature = "gxm", test))]
        if select_macrotiles {
            reserve((inputs.macrotiles.len() * size_of::<macrotile::Candidate>()) as u64)?;
            scratch.macrotiles = inputs.macrotiles.clone();
        }
        let _ = select_macrotiles;
        Ok(scratch)
    }
    /// Replace only inputs on the already selected quality and storage map.
    /// No pixels are allocated and no submitted cache entry is published here.
    pub(crate) fn collect_effect_inputs(
        &self,
        quality: [u32; 2],
        limit: u64,
        pool: &BudgetPool,
        lookup: &mut dyn FnMut(chain::SourceProof<'_>) -> Option<selected::Hit>,
        mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<selected::Plan, GpuError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::OffscreenPlan);
        if self.capture {
            return Ok(selected::Plan::default());
        }
        #[cfg(feature = "profiling")]
        let started = std::time::Instant::now();
        let mut collector = Collector {
            quality,
            pool,
            lookup,
            scans: 0,
        };
        let mut scratch = self.walk(true, true, Some(&mut collector), &mut reserve)?;
        let mut stats = selected::Stats {
            scans: collector.scans,
            candidates: scratch.effect_inputs.len() as u64,
            events: scratch.events.as_ref().unwrap().len() as u64,
            #[cfg(feature = "profiling")]
            scan_ns: started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            ..Default::default()
        };
        if scratch.effect_inputs.is_empty() {
            return Ok(selected::Plan {
                stats,
                ..Default::default()
            }
            .diagnosed());
        }
        let candidates = std::mem::take(&mut scratch.effect_inputs);
        let events = scratch.events.take().unwrap();
        let fixed = scratch
            .coverage_cost(self.coverage_cost)?
            .checked_add(self.selection.persistent_bytes())
            .ok_or(GpuError::InvalidSize)?;
        // Seed every mapped class once; later recounts only reset counters.
        recount(
            &events,
            std::iter::empty(),
            |s| self.schedule.storage_size(s),
            &mut scratch,
            &mut reserve,
        )?;
        let count = candidates.len();
        reserve(
            (count * (size_of::<usize>() * 4 + size_of::<std::ops::Range<usize>>() * 2)) as u64,
        )?;
        let mut skipped = Vec::with_capacity(count * 2);
        #[cfg(feature = "profiling")]
        let recount_started = std::time::Instant::now();
        let chosen = selected::choose(
            &candidates,
            limit,
            &mut stats,
            |chosen| {
                selected::exclusions(&candidates, chosen, &mut skipped);
                recount(
                    &events,
                    skipped.iter().cloned(),
                    |s| self.schedule.storage_size(s),
                    &mut scratch,
                    &mut reserve,
                )?;
                scratch
                    .cost(self.cost)?
                    .checked_add(fixed)
                    .ok_or(GpuError::InvalidSize)
            },
            |_| Ok(()),
        )?;
        #[cfg(feature = "profiling")]
        {
            stats.recount_ns = recount_started
                .elapsed()
                .as_nanos()
                .min(u128::from(u64::MAX)) as u64;
        }
        if chosen.is_empty() {
            return Ok(selected::Plan {
                stats,
                ..Default::default()
            }
            .diagnosed());
        }
        let mut plan = selected::Plan {
            stats,
            fixed_bytes: fixed,
            ..Default::default()
        };
        for &index in &chosen {
            let candidate = &candidates[index];
            super::super::scopes::push_stack(
                &mut plan.entries,
                candidate.entry.clone(),
                &mut reserve,
            )?;
            if !plan.owners.contains(&candidate.owner.owner) {
                super::super::scopes::push_stack(
                    &mut plan.owners,
                    candidate.owner.owner,
                    &mut reserve,
                )?;
                plan.pinned_bytes += candidate.owner.bytes;
            }
        }
        plan.entries.sort_unstable_by_key(|entry| entry.root);
        plan.requirements = Requirements::collect(
            scratch
                .buckets
                .iter()
                .map(|bucket| (bucket.size, bucket.peak)),
            &mut reserve,
        )?
        .with_exact(&scratch.exact, &mut reserve)?;
        plan.bytes = scratch
            .cost(self.cost)?
            .checked_add(fixed)
            .and_then(|v| v.checked_add(plan.pinned_bytes))
            .ok_or(GpuError::InvalidSize)?;
        plan.fixed_bytes += plan.pinned_bytes;
        // The accepted exclusions are already sorted. Build the retained event
        // stream once, and shift unaffected macrotile spans onto that stream.
        selected::exclusions(&candidates, &chosen, &mut skipped);
        reserve((events.len() * size_of::<super::super::target_storage::Event>()) as u64)?;
        plan.events = Vec::with_capacity(events.len());
        let mut exclusions = skipped.iter().peekable();
        for (index, event) in events.iter().enumerate() {
            while exclusions.peek().is_some_and(|range| range.end <= index) {
                exclusions.next();
            }
            if !exclusions
                .peek()
                .is_some_and(|range| range.contains(&index))
            {
                plan.events.push(*event);
            }
        }
        #[cfg(any(feature = "gxm", test))]
        for mut candidate in scratch.macrotiles {
            if chosen.iter().any(|&index| {
                let source = &candidates[index];
                selected::exclusions_overlap(
                    &source.events,
                    source.release,
                    &candidate.events,
                    candidate.release,
                )
            }) {
                continue;
            }
            let shift = |at| {
                skipped
                    .iter()
                    .map(|range| range.end.min(at).saturating_sub(range.start.min(at)))
                    .sum::<usize>()
            };
            candidate.events = candidate.events.start - shift(candidate.events.start)
                ..candidate.events.end - shift(candidate.events.end);
            candidate.release -= shift(candidate.release);
            super::super::scopes::push_stack(&mut plan.macrotiles, candidate, &mut reserve)?;
        }
        Ok(plan.diagnosed())
    }
}
