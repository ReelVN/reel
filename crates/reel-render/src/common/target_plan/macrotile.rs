//! Replace proved ordinary target lifetimes with complete macrotile owners.
use super::*;
use crate::common::{effect_chain, effect_plan, target_storage::Event};
use std::ops::Range;

pub(crate) const OWNER_MAX_CELLS: usize = 16;

mod selection;

/// A source belongs to one exact drawing, command slice and group occurrence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Key {
    drawing: usize,
    commands: usize,
    length: usize,
    root: usize,
}
impl Key {
    pub(crate) fn new(drawing: &FrameDrawing, commands: &[FrameCommand], root: usize) -> Self {
        Self {
            drawing: std::ptr::from_ref(drawing) as usize,
            commands: commands.as_ptr() as usize,
            length: commands.len(),
            root,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Candidate {
    /// Whole physical atlas containing ordered cells and reusable generations.
    pub owner: usize,
    /// Only the final generation may retain its cell contents beyond this frame.
    pub last_use: bool,
    pub first_cell: usize,
    pub owner_cells: usize,
    pub key: Key,
    pub effects: [usize; effect_chain::MAX_CELLS - 1],
    pub count: usize,
    pub logical: [u32; 2],
    pub storage: [u32; 2],
    pub cell: [u32; 2],
    pub grid: [u32; 2],
    pub(super) events: Range<usize>,
    pub(super) release: usize,
    window: Option<(usize, usize)>,
}
impl Candidate {
    pub(crate) fn extent(&self) -> [u32; 2] {
        [self.cell[0] * self.grid[0], self.cell[1] * self.grid[1]]
    }
}
pub(super) struct Open {
    pub depth: usize,
    candidate: Candidate,
}
impl Open {
    pub fn finish(mut self, end: usize) -> Candidate {
        self.candidate.events.end = end;
        self.candidate.release = end;
        self.candidate
    }
}

pub(super) fn open(
    current: &Stream<'_>,
    group: &FrameGroup,
    eligible: bool,
    capture: bool,
    samples: u32,
    schedule: &offscreen::Plan,
    start: usize,
) -> Result<Option<Open>, GpuError> {
    if !eligible
        || capture
        || current.offset != Some(0)
        || current.retained_context != 0
        || !current.drawing.transitions().is_empty()
        || !current.drawing.capture_exclusions().is_empty()
        || !effect_plan::terminal_composite(group, samples, false)
    {
        return Ok(None);
    }
    let root = current.next - 1;
    let Some(chain) = effect_chain::plan(current.commands, root, &current.groups) else {
        return Ok(None);
    };
    let storage = schedule.storage_size(current.size);
    let Some((cell, grid)) = effect_chain::layout(storage, chain.count + 1) else {
        return Ok(None);
    };
    for &push in std::iter::once(&root).chain(chain.effects[..chain.count].iter()) {
        let command = &current.commands[push];
        let FrameOp::PushGroup(group) = &command.op else {
            unreachable!("proved effect group")
        };
        let shader = effect_plan::shader(
            current.drawing,
            group.effect.as_ref().unwrap(),
            command.origin.at,
        )?;
        if group.effect.as_ref().unwrap().resolution_scale != 1.
            || shader.descriptor.output != reel_ui::stage::effect::EffectType::Vec4
            || !shader.descriptor.source_sampler
            || shader.descriptor.textures != 0
        {
            return Ok(None);
        }
    }
    Ok(Some(Open {
        depth: current.scopes.len(),
        candidate: Candidate {
            owner: 0,
            last_use: true,
            first_cell: 0,
            owner_cells: chain.count + 1,
            key: Key::new(current.drawing, current.commands, root),
            effects: chain.effects,
            count: chain.count,
            logical: current.size,
            storage,
            cell,
            grid,
            events: start..start,
            release: usize::MAX,
            window: schedule.window(root),
        },
    }))
}

/// One chosen physical plan; ordinary mappings and logical grids stay unchanged.
pub(crate) struct Selection {
    pub entries: Vec<Candidate>,
    pub requirements: Requirements,
    pub bytes: u64,
}

/// Recoverable preparation releases its complete unsubmitted set before the
/// ordinary plan acquires storage. A release fault aborts the replacement.
pub(crate) fn prepare_or_ordinary<S, T>(
    state: &mut S,
    recoverable: fn(&GpuError) -> bool,
    selected: impl FnOnce(&mut S) -> Result<T, GpuError>,
    rollback: impl FnOnce(&mut S) -> Result<(), GpuError>,
    ordinary: impl FnOnce(&mut S) -> Result<T, GpuError>,
) -> Result<T, GpuError> {
    match selected(state) {
        Ok(value) => Ok(value),
        Err(error) if recoverable(&error) => {
            rollback(state)?;
            ordinary(state)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
fn remaining(
    events: &[Event],
    selected: impl Iterator<Item = Range<usize>>,
    storage: impl Fn([u32; 2]) -> [u32; 2],
    reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
) -> Result<Requirements, GpuError> {
    let mut scratch = Scratch::default();
    recount(events, selected, storage, &mut scratch, reserve)?;
    Requirements::collect(
        scratch
            .buckets
            .iter()
            .map(|bucket| (bucket.size, bucket.peak)),
        reserve,
    )
}

#[cfg(test)]
fn cost(
    requirements: &Requirements,
    bytes: &dyn Fn([u32; 2]) -> Result<u64, GpuError>,
) -> Result<u64, GpuError> {
    requirements.iter().try_fold(0u64, |sum, (size, count)| {
        bytes(size)?
            .checked_mul(count)
            .and_then(|v| sum.checked_add(v))
            .ok_or(GpuError::InvalidSize)
    })
}

impl Input<'_> {
    /// Reuse the exact ordinary traversal, replacing only complete proved spans.
    /// Each atlas remains live through submission and is charged in full once.
    pub(crate) fn macrotile_selection(
        &self,
        limit: u64,
        mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Option<Selection>, GpuError> {
        let inputs = self.schedule.effect_inputs();
        if !inputs.is_empty() {
            let count = inputs.macrotiles.len();
            reserve((count * size_of::<Candidate>()) as u64)?;
            return Selection::choose(
                &inputs.events,
                inputs.macrotiles.clone(),
                |size| self.schedule.storage_size(size),
                self.cost,
                inputs.fixed_bytes,
                limit,
                self.max_dimension,
                &mut reserve,
            );
        }
        if self.capture
            || self.samples != 1
            || self.second.is_some()
            || !self.frame.drawing().transitions().is_empty()
            || !self.frame.drawing().capture_exclusions().is_empty()
        {
            return Ok(None);
        }
        let scratch = self.walk(true, true, None, &mut reserve)?;
        let fixed = scratch
            .coverage_cost(self.coverage_cost)?
            .checked_add(self.selection.persistent_bytes())
            .ok_or(GpuError::InvalidSize)?;
        let events = scratch
            .events
            .as_deref()
            .expect("recorded target lifetimes");
        let mut candidates = scratch.macrotiles;
        candidates.retain(|candidate| candidate.extent().iter().all(|n| *n <= self.max_dimension));
        if candidates.is_empty() {
            return Ok(None);
        }
        Selection::choose(
            events,
            candidates,
            |size| self.schedule.storage_size(size),
            self.cost,
            fixed,
            limit,
            self.max_dimension,
            &mut reserve,
        )
    }
}

#[cfg(test)]
mod frame_tests;
#[cfg(test)]
pub(crate) use frame_tests::warm_terminal_sources;
#[cfg(test)]
mod tests;
