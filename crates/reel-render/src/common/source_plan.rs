//! Eliminate pixel producers whose ordinary color effect replaces their result.
use super::{
    GpuError, effect_plan,
    renderer::{Budgets, Reservation, reserve},
};
use reel_ui::{
    frame::{FrameDrawing, FrameGroup, FrameOp, RecordLocation, RenderFrame},
    kurbo::Affine,
    stage::effect::EffectType,
};

#[derive(Clone, Copy)]
struct Entry {
    drawing: usize,
    push: usize,
    end: usize,
}
/// One preparation-owned selection, shared by demand, admission and replay.
/// Entries retain the group's own command and effect bindings; only its balanced
/// child range is dead. Drawings remain owned by the preparation's input frames.
#[derive(Default)]
pub(crate) struct Plan {
    entries: Vec<Entry>,
    _storage: Option<Reservation>,
}
impl Plan {
    pub(crate) fn collect<'a>(
        frames: impl Iterator<Item = &'a RenderFrame> + Clone,
        capture: bool,
        budgets: &Budgets,
        local: &Budgets,
        limit: u64,
    ) -> Result<Self, GpuError> {
        let mut plan = Self::default();
        if capture {
            return Ok(plan);
        }
        let mut capacity = 0;
        for frame in frames.clone() {
            for entry in Candidates::new(frame.drawing()) {
                entry?;
                capacity += 1;
            }
        }
        if capacity == 0 {
            return Ok(plan);
        }
        plan._storage = Some(reserve(
            &budgets.transient,
            &local.transient,
            (capacity * size_of::<Entry>()) as u64,
            limit,
            "source plan",
        )?);
        plan.entries = Vec::with_capacity(capacity);
        for frame in frames {
            for entry in Candidates::new(frame.drawing()) {
                plan.entries.push(entry?);
            }
        }
        plan.entries
            .sort_unstable_by_key(|entry| (entry.drawing, entry.push));
        plan.entries
            .dedup_by_key(|entry| (entry.drawing, entry.push));
        Ok(plan)
    }
    fn preceding(&self, drawing: &FrameDrawing, command: usize) -> Option<Entry> {
        let drawing = drawing as *const _ as usize;
        let index = self
            .entries
            .partition_point(|entry| (entry.drawing, entry.push) <= (drawing, command));
        index
            .checked_sub(1)
            .map(|index| self.entries[index])
            .filter(|entry| entry.drawing == drawing)
    }
    pub(crate) fn end(&self, drawing: &FrameDrawing, push: usize) -> Option<usize> {
        self.preceding(drawing, push)
            .filter(|entry| entry.push == push)
            .map(|entry| entry.end)
    }
    pub(crate) fn selected(&self, drawing: &FrameDrawing, command: usize) -> bool {
        self.preceding(drawing, command)
            .is_none_or(|entry| command == entry.push || command >= entry.end - 1)
    }
}

/// Count and publish the same outermost balanced ranges without scratch storage.
/// Enclosing coverage can delay a child's effect across its own stores, so the
/// iterator preserves those consumers and never visits their internal candidates.
struct Candidates<'a> {
    drawing: &'a FrameDrawing,
    next: usize,
}
impl<'a> Candidates<'a> {
    fn new(drawing: &'a FrameDrawing) -> Self {
        Self {
            drawing,
            next: if drawing.transitions().is_empty() && drawing.capture_exclusions().is_empty() {
                0
            } else {
                drawing.commands().len()
            },
        }
    }
}
impl Iterator for Candidates<'_> {
    type Item = Result<Entry, GpuError>;
    fn next(&mut self) -> Option<Self::Item> {
        let commands = self.drawing.commands();
        while let Some(command) = commands.get(self.next) {
            let push = self.next;
            self.next += 1;
            if matches!(command.op, FrameOp::PushClip(_)) {
                self.next = scope_end(commands, push).unwrap_or(commands.len());
                continue;
            }
            let FrameOp::PushGroup(group) = &command.op else {
                continue;
            };
            if !ordinary(group) {
                self.next = scope_end(commands, push).unwrap_or(commands.len());
                continue;
            }
            match replaces_source(self.drawing, group, command.origin.at) {
                Err(error) => return Some(Err(error)),
                Ok(false) => continue,
                Ok(true) => {}
            }
            if let Some(end) = scope_end(commands, push) {
                self.next = end;
                return Some(Ok(Entry {
                    drawing: self.drawing as *const _ as usize,
                    push,
                    end,
                }));
            }
        }
        None
    }
}
fn replaces_source(
    drawing: &FrameDrawing,
    group: &FrameGroup,
    at: RecordLocation,
) -> Result<bool, GpuError> {
    if !ordinary(group) {
        return Ok(false);
    }
    let Some(effect) = &group.effect else {
        return Ok(false);
    };
    let shader = effect_plan::shader(drawing, effect, at)?;
    Ok(shader.descriptor.output == EffectType::Vec4 && !shader.descriptor.source_sampler)
}
fn ordinary(group: &FrameGroup) -> bool {
    !(group.clip.is_some()
        || group.mask.is_some()
        || group.image_mask.is_some()
        || group.mask_effect.is_some()
        || !group.filters.is_empty()
        || group.composite.blend != Default::default()
        || group.invert != 0.)
}
fn scope_end(commands: &[reel_ui::frame::FrameCommand], push: usize) -> Option<usize> {
    let mut depth = 1;
    for (index, command) in commands.iter().enumerate().skip(push + 1) {
        match command.op {
            FrameOp::PushGroup(_) | FrameOp::PushClip(_) => depth += 1,
            FrameOp::PopGroup | FrameOp::PopClip => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return Some(index + 1);
        }
    }
    None
}

/// Preserve the existing local source grid even when its pixels have no reader.
pub(crate) fn layout(
    group: &FrameGroup,
    parent: Option<&FrameGroup>,
    transform: Affine,
    size: [u32; 2],
    max_dimension: u32,
    at: RecordLocation,
) -> Result<effect_plan::SourceLayout, GpuError> {
    if effect_plan::same_source_domain(group, parent) {
        Ok(effect_plan::SourceLayout {
            size,
            frame_to_source: transform,
        })
    } else {
        effect_plan::source_layout(
            group.effect.as_ref().expect("planned color effect"),
            group.effect_to_target,
            transform,
            max_dimension,
            at,
        )
    }
}

#[cfg(test)]
#[path = "source_plan_tests.rs"]
mod tests;
