//! Immutable paint retention. No candidate, widget state, task, or input lease escapes.
use super::*;
use crate::resources::{ResourceHandle, ResourceLease};
use std::collections::BTreeSet;

pub type RetainError = BuildError;

#[derive(Clone, Debug)]
pub struct RetainedScene {
    pub(crate) drawing: Arc<FrameDrawing>,
    pub(crate) to_parent: Affine,
    pub(crate) pool: BudgetPool,
}
impl RetainedScene {
    pub fn drawing(&self) -> &FrameDrawing {
        &self.drawing
    }
    pub fn to_parent(&self) -> Affine {
        self.to_parent
    }
    pub fn accounted_bytes(&self) -> u64 {
        self.drawing.accounted_bytes
    }
}
#[derive(Clone, Debug)]
pub struct FrameTransition {
    pub root: Option<WidgetId>,
    pub previous: RetainedScene,
    pub progress: f32,
    pub mask: Option<PaintImageMask>,
}
#[derive(Clone, Debug)]
pub struct SceneTransition {
    pub commands: Range<usize>,
    /// Effective inclusion of the transition target and its ancestors.
    pub capture_included: bool,
    pub previous: RetainedScene,
    pub progress: f32,
    pub mask: Option<FrameImageMask>,
    pub parent_to_window: Affine,
    pub mask_to_window: Affine,
}
fn location(owner: Option<WidgetId>) -> RecordLocation {
    RecordLocation {
        fragment: 0,
        command: 0,
        mask_depth: 0,
        owner: owner.unwrap_or_else(|| WidgetId(std::num::NonZeroU64::new(1).unwrap())),
    }
}
pub(crate) fn error(kind: BuildErrorKind) -> BuildError {
    BuildError {
        at: location(None),
        kind,
    }
}
fn transforms(
    geometry: &PresentedGeometryView,
    root: Option<WidgetId>,
) -> Result<(Affine, Affine), BuildError> {
    let Some(root) = root else {
        return Ok((Affine::IDENTITY, Affine::IDENTITY));
    };
    let record = geometry.get(root).ok_or(BuildError {
        at: location(Some(root)),
        kind: BuildErrorKind::InvalidRecord,
    })?;
    let parent = record
        .parent
        .map_or(Affine::IDENTITY, |i| geometry.records()[i].window_transform);
    Ok((parent, record.window_transform))
}
pub(super) fn compile_transitions(
    input: &[FrameTransition],
    subtrees: &[FrameSubtree],
    body_end: usize,
    geometry: &PresentedGeometryView,
    resources: &ResourceSnapshot,
) -> Result<(Vec<SceneTransition>, Vec<Option<WidgetId>>), BuildError> {
    let mut output = Vec::with_capacity(input.len());
    for transition in input {
        if !transition.progress.is_finite() || !(0. ..=1.).contains(&transition.progress) {
            return Err(error(BuildErrorKind::InvalidRecord));
        }
        let (commands, order, capture_included) = match transition.root {
            Some(widget) => {
                let (index, subtree) = subtrees
                    .iter()
                    .enumerate()
                    .find(|(_, s)| s.widget == widget)
                    .ok_or_else(|| error(BuildErrorKind::InvalidRecord))?;
                if subtree.suppressed_by_ancestor {
                    continue;
                }
                (subtree.commands.clone(), index, subtree.capture_included)
            }
            None => (0..body_end, 0, true),
        };
        let (parent_to_window, mask_to_window) = transforms(geometry, transition.root)?;
        let mask = transition
            .mask
            .as_ref()
            .map(|mask| {
                let images = mask
                    .images
                    .iter()
                    .map(|image| {
                        resources
                            .contains_image(image.handle())
                            .then_some(image.handle())
                            .ok_or_else(|| error(BuildErrorKind::InvalidRecord))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(FrameImageMask {
                    program: mask.program.clone(),
                    images,
                    bounds: mask.bounds,
                    parameters: mask.parameters,
                })
            })
            .transpose()?;
        output.push((
            order,
            transition.root,
            SceneTransition {
                commands,
                capture_included,
                previous: transition.previous.clone(),
                progress: transition.progress,
                mask,
                parent_to_window,
                mask_to_window,
            },
        ));
    }
    output.sort_unstable_by_key(|(order, _, t)| (t.commands.start, t.commands.end, *order));
    for pair in output.windows(2) {
        if pair[0].2.commands.end > pair[1].2.commands.start {
            return Err(error(BuildErrorKind::InvalidRecord));
        }
    }
    Ok(output
        .into_iter()
        .map(|(_, root, transition)| (transition, root))
        .unzip())
}

pub struct RetainBuild {
    source: Arc<FrameDrawing>,
    range: Range<usize>,
    to_parent: Affine,
    next: usize,
    commands: Vec<FrameCommand>,
    capture_exclusions: Vec<Range<usize>>,
    masks: BTreeMap<u32, Arc<FrameMask>>,
    mask_queue: Vec<u32>,
    mask_next: usize,
    handles: BTreeSet<ResourceHandle>,
    leases: Vec<ResourceLease>,
    lease_next: usize,
    transitions: Vec<SceneTransition>,
    transition_selection: Vec<bool>,
    transition_next: usize,
    scopes: Vec<ScopeKind>,
    contexts: usize,
    main_checked: bool,
    pool: BudgetPool,
    ticket: BudgetTicket,
    bytes: u64,
    complete: bool,
}
impl RetainBuild {
    pub fn new(
        frame: &RenderFrame,
        root: Option<WidgetId>,
        limits: BuildLimits,
        pool: &BudgetPool,
    ) -> Result<Self, BuildError> {
        let mut range = match root {
            None => 0..frame.inner.body_end,
            Some(widget) => frame
                .inner
                .subtrees
                .iter()
                .find(|s| s.widget == widget)
                .ok_or_else(|| error(BuildErrorKind::InvalidRecord))?
                .commands
                .clone(),
        };
        let (parent, _) = transforms(frame.geometry(), root)?;
        let to_parent = parent.inverse();
        let parent_invertible = finite(to_parent);
        let to_parent = if parent_invertible {
            to_parent
        } else {
            range.end = range.start;
            Affine::IDENTITY
        };
        let ticket = pool
            .reserve(limits.max_bytes)
            .map_err(|_| error(BuildErrorKind::Bytes))?;
        let exclusion_count = frame
            .drawing()
            .capture_exclusions
            .iter()
            .filter(|r| r.start < range.end && r.end > range.start)
            .count();
        let bytes = size_of::<Self>() as u64
            + size_of::<FrameDrawing>() as u64
            + 2 * size_of::<usize>() as u64
            + (range.len() * size_of::<FrameCommand>()) as u64
            + frame.inner.transition_roots.len() as u64
            + (exclusion_count * size_of::<Range<usize>>()) as u64;
        if bytes > limits.max_bytes {
            return Err(error(BuildErrorKind::Bytes));
        }
        let mut capture_exclusions = Vec::with_capacity(exclusion_count);
        for excluded in &frame.drawing().capture_exclusions {
            let start = excluded.start.max(range.start);
            let end = excluded.end.min(range.end);
            if start < end {
                capture_exclusions.push(start - range.start..end - range.start);
            }
        }
        let commands = Vec::with_capacity(range.len());
        // Numeric command ranges cannot distinguish empty siblings at the same gap.
        // Consult identities only here; the finished drawing retains no widget geometry.
        let transition_selection = frame
            .inner
            .transition_roots
            .iter()
            .map(|target| {
                parent_invertible
                    && match (root, target) {
                        (None, _) => true,
                        (Some(root), Some(target)) => {
                            frame.geometry().contains_in_scope(*target, root)
                        }
                        (Some(_), None) => false,
                    }
            })
            .collect();
        Ok(Self {
            source: frame.inner.drawing.clone(),
            next: range.start,
            range,
            to_parent,
            commands,
            capture_exclusions,
            masks: BTreeMap::new(),
            mask_queue: Vec::new(),
            mask_next: 0,
            handles: BTreeSet::new(),
            leases: Vec::new(),
            lease_next: 0,
            transitions: Vec::new(),
            transition_selection,
            transition_next: 0,
            scopes: Vec::new(),
            contexts: 0,
            main_checked: false,
            pool: pool.clone(),
            ticket,
            bytes,
            complete: false,
        })
    }
    fn charge(&mut self, bytes: u64) -> Result<(), BuildError> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or_else(|| error(BuildErrorKind::Bytes))?;
        if self.bytes > self.ticket.bytes() {
            return Err(error(BuildErrorKind::Bytes));
        }
        Ok(())
    }
    fn inspect(&mut self, command: &FrameCommand) -> Result<(), BuildError> {
        command_resources(command, &mut self.handles, &mut self.mask_queue);
        match command.op {
            FrameOp::PushClip(_) => self.scopes.push(ScopeKind::Clip),
            FrameOp::PushGroup(_) => self.scopes.push(ScopeKind::Group),
            FrameOp::PopClip | FrameOp::PopGroup => {
                let expected = if matches!(command.op, FrameOp::PopClip) {
                    ScopeKind::Clip
                } else {
                    ScopeKind::Group
                };
                if self.scopes.pop() != Some(expected) {
                    return Err(BuildError {
                        at: command.origin.at,
                        kind: BuildErrorKind::InvalidRecord,
                    });
                }
            }
            FrameOp::PushContext(_) => self.contexts += 1,
            FrameOp::PopContext => {
                self.contexts = self
                    .contexts
                    .checked_sub(1)
                    .ok_or_else(|| error(BuildErrorKind::InvalidRecord))?
            }
            _ => {}
        }
        Ok(())
    }
    fn balanced(&self) -> Result<(), BuildError> {
        if !self.scopes.is_empty() || self.contexts != 0 {
            return Err(error(BuildErrorKind::InvalidRecord));
        }
        Ok(())
    }
    fn reference_cost(&self, command: &FrameCommand) -> (u64, u64) {
        let references = resource_handles(command);
        let mut units = 1 + command_bytes(command).div_ceil(8);
        let mut bytes = command_payload_bytes(command);
        for handle in references {
            units += 1 + u64::from(usize::BITS - self.handles.len().leading_zeros());
            if !self.handles.contains(&handle) {
                bytes += 512;
            } // Conservative B-tree node allowance.
        }
        if let FrameOp::PushGroup(group) = &command.op
            && group.mask.is_some()
        {
            bytes += growth_bytes(&self.mask_queue).unwrap_or(usize::MAX) as u64;
        }
        if matches!(command.op, FrameOp::PushClip(_) | FrameOp::PushGroup(_)) {
            bytes += growth_bytes(&self.scopes).unwrap_or(usize::MAX) as u64;
        }
        (units, bytes)
    }
    fn source_command(&self, mask: Option<u32>) -> &FrameCommand {
        match mask {
            None => &self.source.commands[self.next],
            Some(id) => &self.source.masks[&id].commands[self.masks[&id].commands.len()],
        }
    }
    pub fn step(&mut self, b: StepBudget) -> Result<BuildProgress, BuildError> {
        let mut p = BuildProgress::default();
        while !self.complete && p.commands < b.max_commands {
            let action = if self.next < self.range.end {
                RetainAction::Command(None)
            } else if !self.main_checked {
                RetainAction::CheckMain
            } else if self.mask_next < self.mask_queue.len() {
                let id = self.mask_queue[self.mask_next];
                let mask = self
                    .source
                    .masks
                    .get(&id)
                    .ok_or_else(|| error(BuildErrorKind::InvalidRecord))?;
                match self.masks.get(&id) {
                    None => RetainAction::Mask(id, mask.clone()),
                    Some(target) => match mask.commands.get(target.commands.len()) {
                        Some(_) => RetainAction::Command(Some(id)),
                        None => RetainAction::NextMask,
                    },
                }
            } else if self.transition_next < self.source.transitions.len() {
                RetainAction::Transition
            } else if self.lease_next < self.source.resources.leases().len() {
                RetainAction::Lease
            } else {
                RetainAction::Done
            };
            let (units, allocation) = match &action {
                RetainAction::Command(mask) => self.reference_cost(self.source_command(*mask)),
                RetainAction::Mask(_, mask) => (
                    1,
                    1024 + (size_of::<FrameMask>()
                        + 2 * size_of::<usize>()
                        + mask.commands.len() * size_of::<FrameCommand>())
                        as u64,
                ),
                RetainAction::Transition => {
                    if !self.transition_selection[self.transition_next] {
                        (1, 0)
                    } else {
                        let transition = &self.source.transitions[self.transition_next];
                        let count = transition.mask.as_ref().map_or(0, |mask| mask.images.len());
                        (
                            1 + count as u64
                                * (2 + u64::from(usize::BITS - self.handles.len().leading_zeros())),
                            growth_bytes(&self.transitions)
                                .ok_or_else(|| error(BuildErrorKind::Bytes))?
                                as u64
                                + count as u64 * (512 + size_of::<ResourceHandle>() as u64),
                        )
                    }
                }
                RetainAction::Lease => {
                    let allocation = if self
                        .handles
                        .contains(&self.source.resources.leases()[self.lease_next].handle())
                    {
                        growth_bytes(&self.leases).ok_or_else(|| error(BuildErrorKind::Bytes))?
                            as u64
                    } else {
                        0
                    };
                    (
                        1 + u64::from(usize::BITS - self.handles.len().leading_zeros()),
                        allocation,
                    )
                }
                _ => (1, 0),
            };
            if units > b.max_units - p.units || allocation > b.max_bytes - p.bytes {
                p.minimum_units = units;
                p.minimum_bytes = allocation;
                break;
            }
            p.units += units;
            p.bytes += allocation;
            p.commands += 1;
            self.charge(allocation)?;
            match action {
                RetainAction::Command(mask) => {
                    // Copy only after the payload and its work have been admitted.
                    // Pixel/font/video data remain in shared resource leases.
                    let command = self.source_command(mask).clone();
                    if let FrameOp::PushGroup(group) = &command.op
                        && group.mask.is_some()
                    {
                        reserve_slot(&mut self.mask_queue)
                            .map_err(|_| error(BuildErrorKind::Bytes))?;
                    }
                    if matches!(command.op, FrameOp::PushClip(_) | FrameOp::PushGroup(_)) {
                        reserve_slot(&mut self.scopes).map_err(|_| error(BuildErrorKind::Bytes))?;
                    }
                    self.inspect(&command)?;
                    match mask {
                        None => {
                            self.commands.push(command);
                            self.next += 1;
                        }
                        Some(id) => Arc::get_mut(self.masks.get_mut(&id).unwrap())
                            .unwrap()
                            .commands
                            .push(command),
                    }
                }
                RetainAction::Mask(id, mask) => {
                    self.masks.insert(
                        id,
                        Arc::new(FrameMask {
                            mode: mask.mode,
                            commands: Vec::with_capacity(mask.commands.len()),
                        }),
                    );
                }
                RetainAction::NextMask => {
                    self.balanced()?;
                    self.mask_next += 1;
                }
                RetainAction::CheckMain => {
                    self.balanced()?;
                    self.main_checked = true;
                }
                RetainAction::Transition => {
                    let transition = &self.source.transitions[self.transition_next];
                    if self.transition_selection[self.transition_next] {
                        let mut transition = transition.clone();
                        transition.commands.start -= self.range.start;
                        transition.commands.end -= self.range.start;
                        if let Some(mask) = &transition.mask {
                            self.handles.extend(mask.images.iter().copied());
                            self.charge(mask.program.storage_bytes() as u64)?;
                        }
                        reserve_slot(&mut self.transitions)
                            .map_err(|_| error(BuildErrorKind::Bytes))?;
                        self.transitions.push(transition);
                    }
                    self.transition_next += 1;
                }
                RetainAction::Lease => {
                    let lease = &self.source.resources.leases()[self.lease_next];
                    if self.handles.remove(&lease.handle()) {
                        let lease = lease.clone();
                        reserve_slot(&mut self.leases).map_err(|_| error(BuildErrorKind::Bytes))?;
                        self.leases.push(lease);
                    }
                    self.lease_next += 1;
                }
                RetainAction::Done => {
                    if !self.handles.is_empty() {
                        return Err(error(BuildErrorKind::InvalidRecord));
                    }
                    self.complete = true;
                }
            }
        }
        p.complete = self.complete;
        Ok(p)
    }
    pub fn finish(mut self) -> Result<RetainedScene, BuildError> {
        if !self.complete {
            return Err(error(BuildErrorKind::Incomplete));
        }
        self.ticket
            .resize(self.bytes)
            .map_err(|_| error(BuildErrorKind::Bytes))?;
        let resources = ResourceSnapshot::retain(self.leases, &self.pool)
            .map_err(|_| error(BuildErrorKind::Bytes))?;
        Ok(RetainedScene {
            to_parent: self.to_parent,
            pool: self.pool.clone(),
            drawing: Arc::new(FrameDrawing {
                commands: self.commands,
                capture_exclusions: self.capture_exclusions,
                masks: self.masks,
                resources,
                transitions: self.transitions,
                accounted_bytes: self.bytes,
                _ticket: self.ticket,
            }),
        })
    }
}
enum RetainAction {
    Command(Option<u32>),
    Mask(u32, Arc<FrameMask>),
    CheckMain,
    NextMask,
    Transition,
    Lease,
    Done,
}
fn resource_handles(command: &FrameCommand) -> impl Iterator<Item = ResourceHandle> + '_ {
    let (first, second, images) = match &command.op {
        FrameOp::Draw(
            FrameDraw::Fill {
                brush: Brush::Image(brush),
                ..
            }
            | FrameDraw::Stroke {
                brush: Brush::Image(brush),
                ..
            },
        ) => (Some(brush.image.resource), None, &[][..]),
        FrameOp::Draw(FrameDraw::GlyphRun(g)) => (
            Some(g.font),
            match &g.brush {
                Brush::Image(brush) => Some(brush.image.resource),
                _ => None,
            },
            &[][..],
        ),
        FrameOp::PushGroup(g) => (
            None,
            None,
            g.image_mask.as_ref().map_or(&[][..], |mask| &mask.images),
        ),
        FrameOp::External(e) => (Some(e.resource), None, &[][..]),
        _ => (None, None, &[][..]),
    };
    [first, second]
        .into_iter()
        .flatten()
        .chain(images.iter().copied())
        .chain(
            match &command.op {
                FrameOp::PushGroup(group) => Some(group),
                _ => None,
            }
            .into_iter()
            .flat_map(|group| group.effect.iter().chain(group.mask_effect.iter()))
            .flat_map(|effect| std::iter::once(effect.shader).chain(effect.images.iter().copied())),
        )
}
pub(crate) fn command_resources(
    command: &FrameCommand,
    handles: &mut BTreeSet<ResourceHandle>,
    masks: &mut Vec<u32>,
) {
    handles.extend(resource_handles(command));
    if let FrameOp::PushGroup(group) = &command.op
        && let Some(mask) = group.mask
    {
        masks.push(mask.mask);
    }
}
pub(crate) fn command_bytes(command: &FrameCommand) -> u64 {
    size_of::<FrameCommand>() as u64 + command_payload_bytes(command)
}
pub(crate) fn command_payload_bytes(command: &FrameCommand) -> u64 {
    fn geometry(shape: &Geometry) -> usize {
        // The frame compiler reserves the exact path element count.
        match shape {
            Geometry::Path(path) => std::mem::size_of_val(path.elements()),
            _ => 0,
        }
    }
    fn stroke(value: &Stroke) -> usize {
        value.dash_pattern.capacity() * size_of::<f64>()
    }
    fn clip(value: &Clip) -> usize {
        match value {
            Clip::Fill { shape, .. } => geometry(shape),
            Clip::Stroke {
                shape,
                stroke: value,
                ..
            } => geometry(shape) + stroke(value),
        }
    }
    fn brush(value: &FrameBrush) -> usize {
        match value {
            Brush::Gradient(g) => g.stops.capacity() * size_of::<peniko::ColorStop>(),
            _ => 0,
        }
    }
    let payload = match &command.op {
        FrameOp::PushClip(value) => clip(value),
        FrameOp::PushGroup(group) => {
            group.clip.as_ref().map_or(0, clip)
                + group
                    .effect
                    .iter()
                    .chain(group.mask_effect.iter())
                    .map(|effect| {
                        effect.values.capacity() * size_of::<crate::stage::effect::EffectValue>()
                            + effect.images.capacity() * size_of::<ResourceHandle>()
                    })
                    .sum::<usize>()
                + group.filters.capacity() * size_of::<Filter>()
                + group.image_mask.as_ref().map_or(0, |mask| {
                    mask.images.capacity() * size_of::<ResourceHandle>()
                        + mask.program.storage_bytes()
                        + 2 * size_of::<usize>() * (1 + mask.program.sources().len())
                })
        }
        FrameOp::Draw(FrameDraw::Fill {
            shape,
            brush: value,
            ..
        }) => geometry(shape) + brush(value),
        FrameOp::Draw(FrameDraw::Stroke {
            shape,
            stroke: value,
            brush: paint,
            ..
        }) => geometry(shape) + stroke(value) + brush(paint),
        FrameOp::Draw(FrameDraw::GlyphRun(g)) => {
            g.glyphs.capacity() * size_of::<record::Glyph>()
                + g.normalized_coords.capacity() * size_of::<imaging::NormalizedCoord>()
                + match &g.style {
                    Style::Stroke(value) => stroke(value),
                    _ => 0,
                }
                + brush(&g.brush)
        }
        FrameOp::PushContext(context) => {
            let name = match &context.kind {
                FrameContextKind::Named(text) => text.0.capacity(),
                _ => 0,
            };
            let value = match &context.value {
                FrameContextValue::Str(text) => text.0.capacity(),
                _ => 0,
            };
            name + value
                + context
                    .source
                    .as_ref()
                    .map_or(0, |source| source.file.0.capacity())
        }
        _ => 0,
    };
    payload as u64
}
