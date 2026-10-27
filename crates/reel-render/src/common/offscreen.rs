//! Batch independent source-over siblings before their ordered parent composites.
//! Retained transitions, captures, backdrop reads and nested isolation delimit runs.
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

#[derive(Clone, Copy)]
pub(crate) struct Entry {
    pub push: usize,
    pub first: bool,
    pub last: bool,
    pub shared: bool,
    source_size: [u32; 2],
    pub view: SourceView,
}

#[derive(Default)]
pub(crate) struct Plan {
    entries: Vec<Entry>,
    storage: super::target_storage::Plan,
    requirements: super::target_plan::Requirements,
}

impl Plan {
    pub(crate) fn requirements(&self) -> &super::target_plan::Requirements {
        if self.storage.bytes() != 0 {
            self.storage.requirements()
        } else {
            &self.requirements
        }
    }
    pub(crate) fn storage_size(&self, logical: [u32; 2]) -> [u32; 2] {
        self.storage.storage_size(logical)
    }

    /// The second output of a whole-frame crossfade shares storage lifetimes,
    /// while its command indices belong to a different drawing.
    #[cfg(feature = "wgpu")]
    pub(crate) fn disable_batches(&mut self) {
        self.entries.clear();
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
        mut reserve: impl FnMut(u64) -> Result<R, GpuError>,
    ) -> Result<(Self, u64, Vec<R>), GpuError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::OffscreenPlan);
        let transform = Affine::scale_non_uniform(
            f64::from(size[0]) / frame.info().logical_size[0],
            f64::from(size[1]) / frame.info().logical_size[1],
        );
        let (mut plan, mut retained) = if second.is_none() {
            Self::build(
                frame.drawing(),
                transform,
                size,
                samples,
                max_dimension,
                glyphs,
                8,
                capture,
                &mut reserve,
            )?
        } else {
            (Self::default(), Vec::new())
        };
        let mut width = 8;
        let mut shared = true;
        loop {
            if plan.is_empty() {
                plan = Self::default();
                retained.clear();
            }
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
            };
            let mut measured = Some(input.measure(|bytes| {
                traversal.push(reserve(bytes)?);
                Ok(())
            })?);
            let mut bytes = measured.as_ref().unwrap().bytes;
            let mut storage_tickets = Vec::new();
            if bytes > byte_limit && measured.as_ref().unwrap().reusable {
                drop(measured.take());
                traversal.clear();
                let (storage, shared_bytes, tickets) = input.storage(&mut reserve)?;
                bytes = shared_bytes;
                plan.storage = storage;
                storage_tickets = tickets;
            }
            if bytes <= byte_limit || plan.is_empty() {
                if plan.storage.bytes() == 0 {
                    plan.requirements = measured.as_ref().unwrap().requirements(|bytes| {
                        storage_tickets.push(reserve(bytes)?);
                        Ok(())
                    })?;
                    // Capacity reuse is optional. A tight transient budget keeps
                    // the already admitted exact plan and its quality intact.
                    if let Ok(Some((storage, tickets))) = super::target_storage::Plan::capacities(
                        &plan.requirements,
                        size,
                        max_dimension,
                        byte_limit.saturating_sub(selection.persistent_bytes()),
                        cost,
                        &mut reserve,
                    ) {
                        bytes = storage.bytes() + selection.persistent_bytes();
                        plan.requirements = Default::default();
                        storage_tickets = tickets;
                        plan.storage = storage;
                    }
                }
                retained.extend(storage_tickets);
                return Ok((plan, bytes, retained));
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
        let mut start = 0;
        for end in 0..self.entries.len() {
            if !self.entries[end].last {
                continue;
            }
            let entries = &mut self.entries[start..=end];
            for (index, entry) in entries.iter_mut().enumerate() {
                entry.first = index == 0;
                entry.shared = false;
                entry.view = entry.view.placed(entry.source_size, [0; 2]);
            }
            if shared && let Some((size, offsets)) = atlas(entries, max_dimension) {
                for (entry, offset) in entries.iter_mut().zip(offsets) {
                    entry.shared = true;
                    entry.view = entry.view.placed(size, offset);
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
        max_dimension: u32,
        glyphs: &glyph_plan::Plan,
        width: usize,
        capture: bool,
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
        let groups = GroupPlan::new(
            commands,
            Some(0),
            std::iter::empty(),
            std::iter::empty(),
            |bytes| {
                traversal.push(reserve(bytes)?);
                Ok(())
            },
        )?;
        let mut entries = Vec::new();
        let mut scissors = Vec::new();
        let mut scissor = Scissor::full(size);
        let mut run_start = 0;
        let mut next = 0;
        while let Some(command) = commands.get(next) {
            match &command.op {
                FrameOp::PushContext(_) | FrameOp::PopContext => {
                    next += 1;
                }
                FrameOp::PushGroup(group) if groups.can_inline(next) => {
                    push_stack(&mut scissors, scissor, |bytes| {
                        traversal.push(reserve(bytes)?);
                        Ok(())
                    })?;
                    scissor = Scissor::group_bounds(group, transform, size, false)
                        .map_or(scissor, |bounds| scissor.intersect(bounds));
                    next += 1;
                }
                FrameOp::PopGroup => {
                    scissor = scissors.pop().expect("validated inline group");
                    next += 1;
                }
                FrameOp::PushGroup(group) => {
                    let mut end = next + 1;
                    let mut depth = 1;
                    let mut leaf = true;
                    while let Some(command) = commands.get(end) {
                        match &command.op {
                            FrameOp::PushGroup(_) => {
                                depth += 1;
                                leaf &= groups.can_inline(end);
                            }
                            FrameOp::PopGroup => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            FrameOp::PushContext(_) | FrameOp::PopContext => {}
                            FrameOp::Draw(draw) => {
                                leaf &= match draw {
                                    FrameDraw::Fill { composite, .. }
                                    | FrameDraw::Stroke { composite, .. } => {
                                        composite.blend == Default::default()
                                    }
                                    FrameDraw::GlyphRun(run) => {
                                        run.composite.blend == Default::default()
                                    }
                                    FrameDraw::BlurredRoundedRect(_) => false,
                                };
                            }
                            _ => leaf = false,
                        }
                        end += 1;
                    }
                    let view = (leaf && group.composite.blend == Default::default())
                        .then(|| {
                            SourceView::group(
                                group,
                                glyphs.group_ink(group),
                                transform,
                                size,
                                scissor,
                                false,
                                max_dimension,
                            )
                        })
                        .flatten();
                    let folded =
                        inline_group_fill(group, &commands[next + 1..], transform, size, samples)
                            .is_some();
                    if let Some(view) = view.filter(|_| !folded) {
                        push_stack(
                            &mut entries,
                            Entry {
                                push: next,
                                first: false,
                                last: false,
                                shared: false,
                                source_size: view.storage_size,
                                view,
                            },
                            |bytes| {
                                retained.push(reserve(bytes)?);
                                Ok(())
                            },
                        )?;
                        if entries.len() - run_start == width {
                            finish_run(&mut entries, &mut run_start);
                        }
                    } else {
                        finish_run(&mut entries, &mut run_start);
                    }
                    next = end + 1;
                }
                FrameOp::PushClip(_) => {
                    finish_run(&mut entries, &mut run_start);
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
                _ => {
                    finish_run(&mut entries, &mut run_start);
                    next += 1;
                }
            }
        }
        finish_run(&mut entries, &mut run_start);
        Ok((
            Self {
                entries,
                ..Self::default()
            },
            retained,
        ))
    }
}

/// Enumerate the eight possible shelf widths without heap scratch. Only the
/// complete allocation is bucketed; each source retains a transparent border.
fn atlas(entries: &[Entry], max_dimension: u32) -> Option<([u32; 2], [[u32; 2]; 8])> {
    let mut sizes = [[0u32; 2]; 8];
    for (size, entry) in sizes.iter_mut().zip(entries) {
        *size =
            [entry.view.parent_rect.width, entry.view.parent_rect.height].map(|value| value + 2);
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

fn finish_run(entries: &mut Vec<Entry>, start: &mut usize) {
    if entries.len() - *start < 2 {
        entries.truncate(*start);
    } else {
        entries.last_mut().unwrap().last = true;
        *start = entries.len();
    }
}
