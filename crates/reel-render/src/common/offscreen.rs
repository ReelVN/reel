//! Batch independent sources before their ordered parent composites.
//! Each source owns its internal task dependencies. External parent reads,
//! retained transitions and captures delimit independent source windows.
use super::{
    GpuError, glyph_plan,
    scissor::Scissor,
    scopes::{GroupPlan, inline_group_fill, push_stack},
    source_view::SourceView,
};
use reel_ui::{
    frame::{FrameDraw, FrameDrawing, FrameOp, RenderFrame},
    kurbo::Affine,
};
pub(crate) mod reuse;
#[path = "offscreen_schedule.rs"]
mod schedule;
pub(crate) use schedule::ParentStep;

/// Compatible ordinary scratch owners for direct and sampled destination paths.
#[derive(Clone, Copy)]
pub(crate) struct ResidentTargets<'a> {
    pub direct: &'a super::target_plan::Requirements,
    pub sampled: &'a super::target_plan::Requirements,
}
#[cfg(test)]
#[path = "offscreen_blend_tests.rs"]
mod blend_tests;
#[cfg(test)]
#[path = "offscreen_test_support.rs"]
pub(crate) mod test_support;

#[derive(Clone, Copy)]
pub(crate) struct Entry {
    pub push: usize,
    pub end: usize,
    pub first: bool,
    pub last: bool,
    pub shared: bool,
    /// Earlier producer whose complete source pixels are read again in this window.
    pub canonical_push: Option<usize>,
    parent_scissor: Scissor,
    source_size: [u32; 2],
    /// Finite translated placement; `None` preserves the original source domain.
    pub view: Option<SourceView>,
}

#[derive(Default)]
pub(crate) struct Plan {
    effect_inputs: super::effect_inputs::Plan,
    output_crops: super::effect_output::Plan,
    physical_crops: bool,
    crop_disabled: bool,
    entries: Vec<Entry>,
    groups: Option<(usize, usize, std::sync::Arc<super::scopes::Cached>)>,
    storage: super::target_storage::Plan,
    requirements: super::target_plan::Requirements,
    coverage_requirements: super::target_plan::Requirements,
    coverage_enabled: bool,
    stable_backdrop: bool,
    sampled_backdrop: bool,
    reused_sources: bool,
    destination: super::blend::DestinationAccess,
}

impl Plan {
    pub(crate) fn output_crop(&self, push: usize) -> Option<super::source_crop::Crop> {
        self.output_crops.crop(push)
    }
    pub(super) fn collect_output_crops(&self) -> bool {
        self.output_crops.collecting
    }
    pub(crate) fn effect_inputs(&self) -> &super::effect_inputs::Plan {
        &self.effect_inputs
    }
    pub(crate) fn set_effect_inputs(&mut self, inputs: super::effect_inputs::Plan) {
        self.effect_inputs = inputs;
    }
    pub(crate) fn crop_backing(&self, logical: [u32; 2]) -> Option<[u32; 2]> {
        self.physical_crops.then(|| self.storage_size(logical))
    }
    pub(crate) fn source_crops(&self) -> bool {
        !self.crop_disabled
    }
    /// The selected traversal needs a sampled copy of an existing destination.
    /// Native backends can preserve arbitrary integer copy rectangles by choosing
    /// linear storage; current-pixel blends leave this false.
    #[cfg(any(feature = "gxm", test))]
    pub(crate) fn sampled_backdrop(&self) -> bool {
        self.sampled_backdrop
    }
    pub(crate) fn destination(&self) -> super::blend::DestinationAccess {
        self.destination
    }

    pub(crate) fn groups(
        &self,
        commands: &[reel_ui::frame::FrameCommand],
        transitions_enabled: bool,
    ) -> Option<super::scopes::Proof> {
        self.groups
            .as_ref()
            .filter(|(identity, length, _)| {
                transitions_enabled
                    && *identity == commands.as_ptr() as usize
                    && *length == commands.len()
            })
            .map(|(_, _, proof)| super::scopes::Proof::Shared(proof.clone()))
    }
    pub(crate) fn requirements(&self) -> &super::target_plan::Requirements {
        if !self.effect_inputs.is_empty() {
            return self.effect_inputs.requirements();
        }
        if self.storage.bytes() != 0 && !self.physical_crops && !self.reused_sources {
            self.storage.requirements()
        } else {
            &self.requirements
        }
    }
    pub(crate) fn coverage_requirements(&self) -> &super::target_plan::Requirements {
        &self.coverage_requirements
    }
    pub(crate) fn coverage_enabled(&self) -> bool {
        self.coverage_enabled
    }
    pub(crate) fn storage_size(&self, logical: [u32; 2]) -> [u32; 2] {
        self.storage.storage_size(logical)
    }

    /// Backdrop allocation can keep the parent's stable capacity while its copy
    /// still uses the finite source viewport, scissor and integer translation.
    pub(crate) fn backdrop_size(&self, source: [u32; 2], parent: [u32; 2]) -> [u32; 2] {
        if self.stable_backdrop { parent } else { source }
    }

    /// The second output of a whole-frame crossfade shares storage lifetimes,
    /// while its command indices belong to a different drawing.
    #[cfg(feature = "wgpu")]
    pub(crate) fn disable_batches(&mut self) {
        self.entries.clear();
        self.output_crops = Default::default();
    }

    pub(crate) fn entry(&self, push: usize) -> Option<&Entry> {
        self.entries
            .binary_search_by_key(&push, |entry| entry.push)
            .ok()
            .map(|i| &self.entries[i])
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Choose the widest independent batch that fits the physical high-water
    /// budget. Narrowing retains painter order and existing quality settings.
    #[expect(
        clippy::too_many_arguments,
        reason = "one physical frame and its target admission policy"
    )]
    #[cfg(test)]
    pub(crate) fn choose<R>(
        frame: &RenderFrame,
        second: Option<&RenderFrame>,
        size: [u32; 2],
        capture: bool,
        samples: u32,
        max_dimension: u32,
        glyphs: &glyph_plan::Plan,
        byte_limit: u64,
        cost: &dyn Fn([u32; 2]) -> Result<u64, GpuError>,
        masks: super::target_plan::MaskExecution,
        selection: &super::retained::Plan,
        reserve: impl FnMut(u64) -> Result<R, GpuError>,
    ) -> Result<(Self, u64, Vec<R>), GpuError> {
        let (plan, bytes, retained, ()) = Self::choose_with_groups(
            frame,
            second,
            size,
            capture,
            samples,
            max_dimension,
            glyphs,
            byte_limit,
            cost,
            None,
            masks,
            super::blend::DestinationAccess::Sampled,
            selection,
            None,
            None,
            |_, bytes| Ok((bytes, ())),
            reserve,
        )?;
        Ok((plan, bytes, retained))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one physical frame and its target admission policy"
    )]
    pub(crate) fn choose_with_groups<R, T>(
        frame: &RenderFrame,
        second: Option<&RenderFrame>,
        size: [u32; 2],
        capture: bool,
        samples: u32,
        max_dimension: u32,
        glyphs: &glyph_plan::Plan,
        byte_limit: u64,
        cost: &dyn Fn([u32; 2]) -> Result<u64, GpuError>,
        coverage_cost: Option<&dyn Fn([u32; 2]) -> Result<u64, GpuError>>,
        masks: super::target_plan::MaskExecution,
        destination: super::blend::DestinationAccess,
        selection: &super::retained::Plan,
        groups: Option<std::sync::Arc<super::scopes::Cached>>,
        resident: Option<ResidentTargets<'_>>,
        mut collect: impl FnMut(&Self, u64) -> Result<(u64, T), GpuError>,
        mut reserve: impl FnMut(u64) -> Result<R, GpuError>,
    ) -> Result<(Self, u64, Vec<R>, T), GpuError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::OffscreenPlan);
        let transform = Affine::scale_non_uniform(
            f64::from(size[0]) / frame.info().logical_size[0],
            f64::from(size[1]) / frame.info().logical_size[1],
        );
        let mut width = 8;
        #[cfg(test)]
        if test_support::disabled() {
            width = 0;
        }
        let (mut plan, mut retained) = if second.is_none() {
            Self::build(
                frame.drawing(),
                transform,
                size,
                samples,
                destination.for_samples(samples),
                max_dimension,
                glyphs,
                width,
                capture,
                &selection.sources,
                groups.as_ref(),
                &mut reserve,
            )?
        } else {
            (Self::default(), Vec::new())
        };
        let mut shared = true;
        let mut crop_disabled = false;
        loop {
            if plan.is_empty() {
                plan = Self::default();
                retained.clear();
            }
            plan.coverage_enabled = coverage_cost.is_some();
            plan.crop_disabled = crop_disabled;
            plan.destination = destination.for_samples(samples);
            plan.groups = groups.as_ref().map(|groups| {
                (
                    frame.commands().as_ptr() as usize,
                    frame.commands().len(),
                    groups.clone(),
                )
            });
            plan.pack(max_dimension, shared);
            let mut traversal = Vec::new();
            let input = super::target_plan::Input {
                frame,
                second,
                size,
                capture,
                samples,
                max_dimension,
                glyphs,
                schedule: &plan,
                masks,
                selection,
                cost,
                coverage_cost,
            };
            let mut measured = Some(input.measure(|bytes| {
                traversal.push(reserve(bytes)?);
                Ok(())
            })?);
            let sampled_backdrop = measured.as_ref().unwrap().sampled_backdrop;
            let empty_resident = super::target_plan::Requirements::default();
            let resident = resident.map_or(&empty_resident, |owners| {
                if sampled_backdrop {
                    owners.sampled
                } else {
                    owners.direct
                }
            });
            let coverage_bytes = measured.as_ref().unwrap().coverage_bytes;
            let mut coverage_tickets = Vec::new();
            let coverage_requirements =
                measured.as_ref().unwrap().coverage_requirements(|bytes| {
                    coverage_tickets.push(reserve(bytes)?);
                    Ok(())
                })?;
            let mut bytes = measured.as_ref().unwrap().bytes;
            if bytes > byte_limit {
                // A submitted immutable prefix can fit even when reproducing
                // its complete cold scratch chain cannot. Resolve it on this
                // unchanged map before storage folding or batch/raster fallback.
                let (selected_bytes, inputs) = collect(&plan, bytes)?;
                if selected_bytes <= byte_limit {
                    plan.sampled_backdrop = sampled_backdrop;
                    plan.coverage_requirements = coverage_requirements;
                    plan.requirements = measured.as_ref().unwrap().requirements(|bytes| {
                        retained.push(reserve(bytes)?);
                        Ok(())
                    })?;
                    retained.extend(coverage_tickets);
                    return Ok((plan, selected_bytes, retained, inputs));
                }
                drop(inputs);
            }
            let share_backdrops = bytes <= byte_limit
                && measured.as_ref().unwrap().reusable
                && measured.as_ref().unwrap().local_backdrop;
            let single_backdrop_class = bytes <= byte_limit
                && !measured.as_ref().unwrap().reusable
                && measured.as_ref().unwrap().local_backdrop;
            let mut storage_tickets = Vec::new();
            if bytes > byte_limit && measured.as_ref().unwrap().reusable {
                if measured.as_ref().unwrap().cropped && !crop_disabled {
                    // Storage folding changes the exact dyadic texture extent.
                    // Remeasure the ordinary path before admitting that fallback.
                    crop_disabled = true;
                    continue;
                }
                drop(measured.take());
                traversal.clear();
                let (storage, shared_bytes, tickets) = input.storage(&mut reserve)?;
                bytes = shared_bytes;
                plan.storage = storage;
                storage_tickets = tickets;
            }
            if bytes <= byte_limit || plan.is_empty() {
                plan.sampled_backdrop = sampled_backdrop;
                plan.coverage_requirements = coverage_requirements;
                if plan.storage.bytes() == 0 {
                    plan.requirements = measured.as_ref().unwrap().requirements(|bytes| {
                        storage_tickets.push(reserve(bytes)?);
                        Ok(())
                    })?;
                }
                // Capacity reuse is optional for both exact and lifetime-shared
                // classes. Metadata pressure leaves the admitted plan intact.
                let capacity_limit = byte_limit
                    .saturating_sub(selection.persistent_bytes())
                    .saturating_sub(coverage_bytes);
                let padded = if plan.storage.bytes() == 0 {
                    super::target_storage::Plan::capacities(
                        &plan.requirements,
                        size,
                        max_dimension,
                        capacity_limit,
                        cost,
                        resident,
                        &mut reserve,
                    )
                } else {
                    plan.storage.padded_capacities(
                        size,
                        max_dimension,
                        capacity_limit,
                        cost,
                        resident,
                        &mut reserve,
                    )
                };
                if let Ok(Some((storage, tickets))) = padded {
                    bytes = storage.bytes() + selection.persistent_bytes() + coverage_bytes;
                    plan.requirements = Default::default();
                    storage_tickets = tickets;
                    plan.storage = storage;
                }
                // A cropped backdrop duplicates its source's size class. Compare
                // the whole padded pool, even below the admission limit: ordered
                // blends can otherwise retain two targets for every source size.
                if share_backdrops {
                    let input = super::target_plan::Input {
                        frame,
                        second,
                        size,
                        capture,
                        samples,
                        max_dimension,
                        glyphs,
                        schedule: &plan,
                        masks,
                        selection,
                        cost,
                        coverage_cost,
                    };
                    if let Ok((mut storage, _, mut tickets)) = input.storage(&mut reserve) {
                        if let Ok(Some((padded, padded_tickets))) = storage.padded_capacities(
                            size,
                            max_dimension,
                            capacity_limit,
                            cost,
                            resident,
                            &mut reserve,
                        ) {
                            storage = padded;
                            tickets = padded_tickets;
                        }
                        let shared_bytes =
                            storage.bytes() + selection.persistent_bytes() + coverage_bytes;
                        // Below pressure, eliminate existing allocations rather
                        // than introducing a new class or higher multiplicity.
                        // Otherwise animated overlap can alternate two pools and
                        // increase creation despite a smaller individual plan.
                        let reuses_pool = storage
                            .requirements()
                            .iter()
                            .all(|(size, count)| count <= plan.requirements().count(size));
                        if shared_bytes < bytes && reuses_pool {
                            bytes = shared_bytes;
                            plan.requirements = Default::default();
                            plan.storage = storage;
                            storage_tickets = tickets;
                        }
                    }
                }
                // With one finite scratch class, source and backdrop overlap.
                // An affordable parent allocation avoids duplicating every
                // source-size bucket over time. Limit its physical cost to two
                // source capacities so a small control cannot retain a screen.
                let stable_capacity = if single_backdrop_class {
                    let parent_bytes = cost(size)?;
                    plan.requirements()
                        .iter()
                        .find(|(storage, _)| *storage != size)
                        .map(|(storage, _)| cost(storage))
                        .transpose()?
                        .is_some_and(|source| parent_bytes <= source.saturating_mul(2))
                } else {
                    false
                };
                if stable_capacity {
                    plan.stable_backdrop = true;
                    let candidate = (|| -> Result<_, GpuError> {
                        let mut temporary = Vec::new();
                        let measured = super::target_plan::Input {
                            frame,
                            second,
                            size,
                            capture,
                            samples,
                            max_dimension,
                            glyphs,
                            schedule: &plan,
                            masks,
                            selection,
                            cost,
                            coverage_cost,
                        }
                        .measure(|bytes| {
                            temporary.push(reserve(bytes)?);
                            Ok(())
                        })?;
                        if measured.bytes > byte_limit {
                            return Ok(None);
                        }
                        let mut tickets = Vec::new();
                        let requirements = measured.requirements(|bytes| {
                            tickets.push(reserve(bytes)?);
                            Ok(())
                        })?;
                        let padded = super::target_storage::Plan::capacities(
                            &requirements,
                            size,
                            max_dimension,
                            capacity_limit,
                            cost,
                            resident,
                            &mut reserve,
                        );
                        if let Ok(Some((storage, padded_tickets))) = padded {
                            let bytes =
                                storage.bytes() + selection.persistent_bytes() + coverage_bytes;
                            Ok(Some((storage, Default::default(), bytes, padded_tickets)))
                        } else {
                            Ok(Some((
                                Default::default(),
                                requirements,
                                measured.bytes,
                                tickets,
                            )))
                        }
                    })();
                    if let Ok(Some((storage, requirements, selected_bytes, tickets))) = candidate {
                        plan.storage = storage;
                        plan.requirements = requirements;
                        bytes = selected_bytes;
                        storage_tickets = tickets;
                    } else {
                        plan.stable_backdrop = false;
                    }
                }
                retained.extend(storage_tickets);
                retained.extend(coverage_tickets);
                plan.groups = groups.map(|groups| {
                    (
                        frame.commands().as_ptr() as usize,
                        frame.commands().len(),
                        groups,
                    )
                });
                // Freeze ordinary capacities first: their rounded denominators
                // are part of the original source sampling phase. Only then
                // replace proved raw writes with exact physical backing slices.
                if !capture && samples == 1 && second.is_none() && !crop_disabled {
                    drop(measured.take());
                    traversal.clear();
                    plan.physical_crops = true;
                    plan.output_crops.begin();
                    let mut temporary = Vec::new();
                    let candidate = super::target_plan::Input {
                        frame,
                        second,
                        size,
                        capture,
                        samples,
                        max_dimension,
                        glyphs,
                        schedule: &plan,
                        masks,
                        selection,
                        cost,
                        coverage_cost,
                    }
                    .physical_measure(|bytes| {
                        temporary.push(reserve(bytes)?);
                        Ok(())
                    });
                    let mut selected = Vec::new();
                    let candidate = candidate.and_then(|m| {
                        if !m.cropped || m.bytes > bytes || m.bytes > byte_limit {
                            return Ok(None);
                        }
                        let requirements = m.requirements(|bytes| {
                            selected.push(reserve(bytes)?);
                            Ok(())
                        })?;
                        let bytes = m.bytes;
                        let mut crops = m.output_crops();
                        if !crops.is_empty() {
                            selected.push(reserve(
                                (crops.capacity() * size_of::<super::effect_output::Entry>())
                                    as u64,
                            )?);
                        }
                        crops.sort_unstable_by_key(|entry| entry.push);
                        Ok(Some((requirements, bytes, crops)))
                    });
                    if let Ok(Some((requirements, selected_bytes, crops))) = candidate {
                        plan.requirements = requirements;
                        bytes = selected_bytes;
                        plan.output_crops.entries = crops;
                        retained.extend(selected);
                    } else {
                        plan.physical_crops = false;
                        plan.output_crops.entries.clear();
                    }
                    plan.output_crops.collecting = false;
                }
                if !capture && plan.reuse_sources(frame.drawing(), selection) {
                    // Keep the frozen raster/storage map. Only production
                    // lifetimes disappear; every original parent read remains.
                    let mut temporary = Vec::new();
                    let measured = super::target_plan::Input {
                        frame,
                        second,
                        size,
                        capture,
                        samples,
                        max_dimension,
                        glyphs,
                        schedule: &plan,
                        masks,
                        selection,
                        cost,
                        coverage_cost,
                    }
                    .physical_measure(|bytes| {
                        temporary.push(reserve(bytes)?);
                        Ok(())
                    })?;
                    plan.requirements = measured.requirements(|bytes| {
                        retained.push(reserve(bytes)?);
                        Ok(())
                    })?;
                    bytes = measured.bytes;
                    plan.reused_sources = true;
                }
                let (bytes, inputs) = collect(&plan, bytes)?;
                return Ok((plan, bytes, retained, inputs));
            }
            plan.storage = super::target_storage::Plan::default();
            if shared && plan.entries.iter().any(|entry| entry.shared) {
                shared = false;
                continue;
            }
            width /= 2;
            plan.narrow(width);
            shared = true;
        }
    }

    /// Pack independent sources into disjoint physical regions. A transparent
    /// texel surrounds each region; sampling preserves the original pixel grid.
    fn pack(&mut self, max_dimension: u32, shared: bool) {
        #[cfg(test)]
        let shared = shared && !test_support::atlas_disabled();
        #[cfg(feature = "profiling")]
        let shared = shared && crate::profiling::shared_source_atlas();
        #[cfg(not(feature = "profiling"))]
        let shared = shared && crate::SHARED_SOURCE_ATLAS_DEFAULT;
        let mut start = 0;
        for end in 0..self.entries.len() {
            if !self.entries[end].last {
                continue;
            }
            let entries = &mut self.entries[start..=end];
            for (index, entry) in entries.iter_mut().enumerate() {
                entry.first = index == 0;
                entry.shared = false;
                entry.view = entry
                    .view
                    .map(|view| view.placed(entry.source_size, [0; 2]));
            }
            if shared && let Some((size, offsets)) = atlas(entries, max_dimension) {
                for (entry, offset) in entries.iter_mut().zip(offsets) {
                    entry.shared = true;
                    entry.view = entry.view.map(|view| view.placed(size, offset));
                }
            }
            start = end + 1;
        }
    }

    fn narrow(&mut self, width: usize) {
        if width < 2 {
            self.entries.clear();
            return;
        }
        let mut output = 0;
        let mut chunk = 0;
        for index in 0..self.entries.len() {
            let mut entry = self.entries[index];
            let end = entry.last;
            entry.last = false;
            self.entries[output] = entry;
            output += 1;
            chunk += 1;
            if chunk == width || end {
                if chunk < 2 {
                    output -= 1;
                } else {
                    self.entries[output - 1].last = true;
                }
                chunk = 0;
            }
        }
        self.entries.truncate(output);
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one physical drawing and its retained scope dependencies"
    )]
    fn build<R>(
        drawing: &FrameDrawing,
        transform: Affine,
        size: [u32; 2],
        samples: u32,
        destination: super::blend::DestinationAccess,
        max_dimension: u32,
        glyphs: &glyph_plan::Plan,
        width: usize,
        capture: bool,
        sources: &super::source_plan::Plan,
        proof: Option<&std::sync::Arc<super::scopes::Cached>>,
        mut reserve: impl FnMut(u64) -> Result<R, GpuError>,
    ) -> Result<(Self, Vec<R>), GpuError> {
        if width < 2
            || !drawing.transitions().is_empty()
            || capture && !drawing.capture_exclusions().is_empty()
        {
            return Ok((Self::default(), Vec::new()));
        }
        let mut retained = Vec::new();
        let mut traversal = Vec::new();
        let commands = drawing.commands();
        let groups = if let Some(proof) = proof {
            super::scopes::Proof::Shared(proof.clone())
        } else {
            super::scopes::Proof::Owned(GroupPlan::new(
                commands,
                Some(0),
                std::iter::empty(),
                std::iter::empty(),
                |bytes| {
                    traversal.push(reserve(bytes)?);
                    Ok(())
                },
            )?)
        };
        let mut entries = Vec::new();
        let mut scissors = Vec::new();
        let mut scissor = Scissor::full(size);
        let mut run_start = 0;
        let mut parent_draw = false;
        let mut next = 0;
        while let Some(command) = commands.get(next) {
            if matches!(command.op, FrameOp::PushGroup(_))
                && let Some(end) = sources.end(drawing, next)
            {
                // Replay emits this effect directly into the parent and skips
                // its children. It cannot participate in a deferred source run.
                finish_run(&mut entries, &mut run_start, &mut parent_draw);
                next = end;
                continue;
            }
            // The build entry guard already excluded active retained/capture boundaries.
            if let FrameOp::PushGroup(group) = &command.op
                && let Some(consumed) = super::scopes::invisible_group(
                    group,
                    &commands[next + 1..],
                    transform,
                    size,
                    scissor,
                    false,
                )
            {
                next += consumed + 1;
                continue;
            }
            match &command.op {
                FrameOp::PushContext(_) | FrameOp::PopContext => {
                    next += 1;
                }
                FrameOp::PushGroup(group) if groups.can_inline(next) => {
                    if parent_draw {
                        finish_run(&mut entries, &mut run_start, &mut parent_draw);
                    }
                    push_stack(&mut scissors, (scissor, false), |bytes| {
                        traversal.push(reserve(bytes)?);
                        Ok(())
                    })?;
                    scissor = Scissor::group_bounds(group, transform, size, false)
                        .map_or(scissor, |bounds| scissor.intersect(bounds));
                    next += 1;
                }
                FrameOp::PopGroup => {
                    let (parent_scissor, isolated) = scissors.pop().expect("validated group");
                    if parent_draw || isolated {
                        finish_run(&mut entries, &mut run_start, &mut parent_draw);
                    }
                    scissor = parent_scissor;
                    next += 1;
                }
                FrameOp::PushGroup(group) => {
                    let mut end = next + 1;
                    let mut depth = 1;
                    let mut independent = true;
                    while let Some(command) = commands.get(end) {
                        match &command.op {
                            FrameOp::PushGroup(child) => {
                                depth += 1;
                                // Every nested target starts inside this isolated
                                // source. Its effects and local backdrop reads
                                // retain their original consumer order; none can
                                // observe the outer source's parent target.
                                independent &= child.filters.is_empty();
                            }
                            FrameOp::PopGroup => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            FrameOp::PushContext(_)
                            | FrameOp::PopContext
                            | FrameOp::PushClip(_)
                            | FrameOp::PopClip => {}
                            FrameOp::Draw(draw) => {
                                independent &= ordinary_draw(draw);
                            }
                            _ => independent = false,
                        }
                        end += 1;
                    }
                    let current_pixel = destination.current_pixel(
                        group.composite.blend,
                        true,
                        group.clip.is_some()
                            || group.mask.is_some()
                            || group.mask_effect.is_some()
                            || group.image_mask.is_some(),
                    ) && group.filters.is_empty();
                    let source_view = SourceView::group(
                        group,
                        glyphs.group_ink(group),
                        transform,
                        size,
                        scissor,
                        false,
                        max_dimension,
                    );
                    let view = (independent
                        && (group.composite.blend == Default::default() || current_pixel))
                        .then_some(source_view)
                        .flatten();
                    let folded =
                        inline_group_fill(group, &commands[next + 1..], transform, size, samples)
                            .is_some();
                    let terminal = independent
                        && super::effect_plan::terminal_composite(group, samples, false);
                    let source_size = if terminal {
                        let source = super::effect_plan::source_layout(
                            group.effect.as_ref().unwrap(),
                            group.effect_to_target,
                            transform,
                            max_dimension,
                            command.origin.at,
                        )?;
                        source.size
                    } else {
                        view.map_or(size, |view| view.storage_size)
                    };
                    if independent && !folded && (view.is_some() || current_pixel || terminal) {
                        push_stack(
                            &mut entries,
                            Entry {
                                push: next,
                                end: end + 1,
                                first: false,
                                last: false,
                                shared: false,
                                canonical_push: None,
                                parent_scissor: scissor,
                                source_size,
                                view,
                            },
                            |bytes| {
                                retained.push(reserve(bytes)?);
                                Ok(())
                            },
                        )?;
                        if entries.len() - run_start == width {
                            finish_run(&mut entries, &mut run_start, &mut parent_draw);
                        }
                    } else {
                        finish_run(&mut entries, &mut run_start, &mut parent_draw);
                        // A full-size ordinary isolation boundary keeps the same
                        // source grid. Schedule its children in that target's
                        // own window; the parent itself has no pending source.
                        if !folded
                            && source_view.is_none()
                            && group.composite.blend == Default::default()
                            && group.effect.is_none()
                            && group.clip.is_none()
                            && group.mask.is_none()
                            && group.mask_effect.is_none()
                            && group.image_mask.is_none()
                            && group.filters.is_empty()
                        {
                            push_stack(&mut scissors, (scissor, true), |bytes| {
                                traversal.push(reserve(bytes)?);
                                Ok(())
                            })?;
                            scissor = Scissor::group_bounds(group, transform, size, false)
                                .map_or(scissor, |bounds| scissor.intersect(bounds));
                            next += 1;
                            continue;
                        }
                    }
                    next = end + 1;
                }
                FrameOp::PushClip(clip) => {
                    finish_run(&mut entries, &mut run_start, &mut parent_draw);
                    if let Some(bounds) = Scissor::from_clip(clip, transform, size, samples) {
                        push_stack(&mut scissors, (scissor, true), |bytes| {
                            traversal.push(reserve(bytes)?);
                            Ok(())
                        })?;
                        scissor = scissor.intersect(bounds);
                        next += 1;
                        continue;
                    }
                    let mut depth = 1;
                    next += 1;
                    while let Some(command) = commands.get(next) {
                        match &command.op {
                            FrameOp::PushClip(_) => depth += 1,
                            FrameOp::PopClip => {
                                depth -= 1;
                                if depth == 0 {
                                    next += 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                        next += 1;
                    }
                }
                FrameOp::PopClip => {
                    finish_run(&mut entries, &mut run_start, &mut parent_draw);
                    scissor = scissors.pop().expect("validated scissor clip").0;
                    next += 1;
                }
                FrameOp::Draw(draw) if ordinary_draw(draw) => {
                    parent_draw |= entries.len() > run_start;
                    next += 1;
                }
                _ => {
                    finish_run(&mut entries, &mut run_start, &mut parent_draw);
                    next += 1;
                }
            }
        }
        finish_run(&mut entries, &mut run_start, &mut parent_draw);
        Ok((
            Self {
                entries,
                ..Self::default()
            },
            retained,
        ))
    }
}

fn ordinary_draw(draw: &FrameDraw) -> bool {
    match draw {
        FrameDraw::Fill { composite, .. } | FrameDraw::Stroke { composite, .. } => {
            composite.blend == Default::default()
        }
        FrameDraw::GlyphRun(run) => run.composite.blend == Default::default(),
        FrameDraw::Sprites(sprites) => sprites.composite.blend == Default::default(),
        FrameDraw::BlurredRoundedRect(_) => false,
    }
}

/// Enumerate the eight possible shelf widths without heap scratch. Only the
/// complete allocation is bucketed; each source retains a transparent border.
fn atlas(entries: &[Entry], max_dimension: u32) -> Option<([u32; 2], [[u32; 2]; 8])> {
    let mut sizes = [[0u32; 2]; 8];
    for (size, entry) in sizes.iter_mut().zip(entries) {
        let view = entry.view?;
        *size = [view.parent_rect.width, view.parent_rect.height].map(|value| value + 2);
    }
    let sizes = &sizes[..entries.len()];
    let mut best = None;
    // The widest individual footprint must fit in every candidate shelf.
    let minimum = sizes.iter().map(|s| s[0]).max()?;
    let mut width = 0;
    for size in sizes {
        width += size[0];
        let limit = width.max(minimum).min(max_dimension);
        let (mut x, mut y, mut row, mut used) = (0, 0, 0, 0);
        let mut offsets = [[0; 2]; 8];
        let mut fits = true;
        for (index, &[w, h]) in sizes.iter().enumerate() {
            if w > limit {
                fits = false;
                break;
            }
            if x + w > limit {
                y += row;
                x = 0;
                row = 0;
            }
            offsets[index] = [x + 1, y + 1];
            x += w;
            row = row.max(h);
            used = used.max(x);
        }
        let size = [used, y + row].map(|value| value.div_ceil(32) * 32);
        if fits && size.into_iter().all(|value| value <= max_dimension) {
            let pixels = u64::from(size[0]) * u64::from(size[1]);
            if best.as_ref().is_none_or(|(old, _): &([u32; 2], _)| {
                (pixels, size[0].max(size[1]))
                    < (u64::from(old[0]) * u64::from(old[1]), old[0].max(old[1]))
            }) {
                best = Some((size, offsets));
            }
        }
    }
    best
}

fn finish_run(entries: &mut Vec<Entry>, start: &mut usize, parent_draw: &mut bool) {
    *parent_draw = false;
    if entries.len() - *start < 2 {
        entries.truncate(*start);
    } else {
        entries.last_mut().unwrap().last = true;
        *start = entries.len();
    }
}
