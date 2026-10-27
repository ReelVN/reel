//! Retained GPU inputs and the exact command frontier of one preparation phase.
#[cfg(test)]
mod tests;
use super::{
    GpuError,
    renderer::{Budgets, Reservation, reserve},
    scopes::push_stack,
};
use reel_core::budget::{BudgetPool, BudgetTicket};
use reel_ui::bridge::CandidateKey;
use reel_ui::frame::{
    FrameCommand, FrameDraw, FrameDrawing, FrameOp, LocalId, RecordingSource, RenderFrame,
    RetainedScene, SceneTransition,
};
use std::{
    ops::Range,
    sync::{Arc, Mutex},
};

/// The parent prefix is part of the source: retained blend operations read it.
/// Serialized commands contain resource handles and paint metadata, never pixels.
/// Strong retained owners prevent process-local drawing identities being reused.
pub(crate) struct Key {
    parent: Option<Arc<Key>>,
    previous: RetainedScene,
    _prior: Vec<RetainedScene>,
    bytes: Vec<u8>,
    recordings: Vec<(RecordingSource, LocalId)>,
    anchor: usize,
    _reservation: Reservation,
}
impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        let (mut left, mut right) = (self, other);
        loop {
            if !std::ptr::eq(left.previous.drawing(), right.previous.drawing())
                || left.bytes != right.bytes
                || left.recordings != right.recordings
            {
                return false;
            }
            match (&left.parent, &right.parent) {
                (Some(a), Some(b)) if Arc::ptr_eq(a, b) => return true,
                (Some(a), Some(b)) => {
                    left = a;
                    right = b;
                }
                (None, None) => return true,
                _ => return false,
            }
        }
    }
}
impl Eq for Key {}
impl Drop for Key {
    fn drop(&mut self) {
        let mut parent = self.parent.take();
        while let Some(shared) = parent {
            match Arc::try_unwrap(shared) {
                Ok(mut key) => parent = key.parent.take(),
                Err(_) => break,
            }
        }
    }
}
#[derive(serde::Serialize)]
enum PaintDependency<'a> {
    Raw(&'a FrameOp),
    Recording(usize, Projection<'a>),
    Context,
}
#[derive(serde::Serialize)]
enum Projection<'a> {
    Clip([f64; 6]),
    Draw {
        kind: u8,
        transform: [f64; 6],
        brush_transform: Option<[f64; 6]>,
        image: Option<&'a reel_ui::peniko::ImageBrush<reel_ui::frame::FrameImage>>,
        font: Option<reel_ui::resources::ResourceHandle>,
    },
}
fn projection(command: &FrameCommand) -> Option<Projection<'_>> {
    use reel_ui::{imaging::record::Clip, peniko::Brush};
    fn image(
        brush: &reel_ui::frame::FrameBrush,
    ) -> Option<&reel_ui::peniko::ImageBrush<reel_ui::frame::FrameImage>> {
        if let Brush::Image(image) = brush {
            Some(image)
        } else {
            None
        }
    }
    Some(match &command.op {
        FrameOp::PushClip(Clip::Fill { transform, .. } | Clip::Stroke { transform, .. }) => {
            Projection::Clip(transform.as_coeffs())
        }
        FrameOp::Draw(FrameDraw::Fill {
            transform,
            brush_transform,
            brush,
            ..
        }) => Projection::Draw {
            kind: 0,
            transform: transform.as_coeffs(),
            brush_transform: brush_transform.map(|t| t.as_coeffs()),
            image: image(brush),
            font: None,
        },
        FrameOp::Draw(FrameDraw::Stroke {
            transform,
            brush_transform,
            brush,
            ..
        }) => Projection::Draw {
            kind: 1,
            transform: transform.as_coeffs(),
            brush_transform: brush_transform.map(|t| t.as_coeffs()),
            image: image(brush),
            font: None,
        },
        FrameOp::Draw(FrameDraw::GlyphRun(run)) => Projection::Draw {
            kind: 2,
            transform: run.transform.as_coeffs(),
            brush_transform: run.brush_transform.map(|t| t.as_coeffs()),
            image: image(&run.brush),
            font: Some(run.font),
        },
        FrameOp::Draw(FrameDraw::BlurredRoundedRect(draw)) => Projection::Draw {
            kind: 3,
            transform: draw.transform.as_coeffs(),
            brush_transform: None,
            image: None,
            font: None,
        },
        _ => return None,
    })
}
impl Key {
    fn same_use(&self, other: &Self) -> bool {
        let (mut left, mut right) = (self, other);
        loop {
            if left.anchor != right.anchor {
                return false;
            }
            match (&left.parent, &right.parent) {
                (Some(a), Some(b)) => {
                    left = a;
                    right = b;
                }
                (None, None) => return true,
                _ => return false,
            }
        }
    }
    fn new(
        drawing: &FrameDrawing,
        transition: &SceneTransition,
        parent: Option<Arc<Key>>,
        domain: ([u32; 2], [f64; 2]),
        budgets: &Budgets,
        local: &Budgets,
        limit: u64,
    ) -> Result<Arc<Self>, GpuError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::RetainedPlan);
        let count = drawing
            .transitions()
            .iter()
            .position(|candidate| std::ptr::eq(candidate, transition))
            .expect("retained dependency belongs to this drawing");
        let prefix = &drawing.commands()[..transition.commands.start];
        let paths = prefix
            .iter()
            .filter_map(|command| drawing.recording(command.origin))
            .map(|(source, _)| std::mem::size_of_val(source.mask_path()))
            .sum::<usize>();
        let temporary = reserve(
            &budgets.transient,
            &local.transient,
            (count
                * size_of::<(
                    usize,
                    f32,
                    Option<&reel_ui::frame::FrameImageMask>,
                    [f64; 6],
                    [f64; 6],
                    [f64; 6],
                    Range<usize>,
                )>()
                + prefix.len()
                    * (size_of::<PaintDependency<'_>>() + size_of::<(RecordingSource, LocalId)>())
                + paths) as u64,
            limit,
            "retained dependencies",
        )?;
        let mut recordings = Vec::with_capacity(prefix.len());
        let dependencies = prefix
            .iter()
            .map(|command| {
                if matches!(command.op, FrameOp::PushContext(_) | FrameOp::PopContext) {
                    return PaintDependency::Context;
                }
                if let Some(projection) = projection(command)
                    && let Some((source, local)) = drawing.recording(command.origin)
                {
                    let index = recordings.len();
                    recordings.push((source.clone(), local));
                    PaintDependency::Recording(index, projection)
                } else {
                    PaintDependency::Raw(&command.op)
                }
            })
            .collect::<Vec<_>>();
        let prior = drawing.transitions()[..count]
            .iter()
            .map(|t| {
                (
                    t.previous.drawing() as *const _ as usize,
                    t.progress,
                    t.mask.as_ref(),
                    t.parent_to_window.as_coeffs(),
                    t.previous.to_parent().as_coeffs(),
                    t.mask_to_window.as_coeffs(),
                    t.commands.clone(),
                )
            })
            .collect::<Vec<_>>();
        let descriptor = (
            domain,
            transition.parent_to_window.as_coeffs(),
            transition.previous.to_parent().as_coeffs(),
            &dependencies,
            drawing.masks(),
            &prior,
        );
        let size = postcard::experimental::serialized_size(&descriptor)
            .map_err(|_| GpuError::InvalidSize)?;
        let reservation = reserve(
            &budgets.transient,
            &local.transient,
            (size
                + size_of::<Self>()
                + count * size_of::<RetainedScene>()
                + recordings.capacity() * size_of::<(RecordingSource, LocalId)>()
                + recordings
                    .iter()
                    .map(|(source, _)| std::mem::size_of_val(source.mask_path()))
                    .sum::<usize>()) as u64,
            limit,
            "retained dependencies",
        )?;
        let mut bytes = vec![0; size];
        postcard::to_slice(&descriptor, &mut bytes).map_err(|_| GpuError::InvalidSize)?;
        drop(dependencies);
        drop(prior);
        drop(temporary);
        Ok(Arc::new(Self {
            parent,
            previous: transition.previous.clone(),
            _prior: drawing.transitions()[..count]
                .iter()
                .map(|t| t.previous.clone())
                .collect(),
            bytes,
            recordings,
            anchor: transition as *const _ as usize,
            _reservation: reservation,
        }))
    }
}

pub(crate) struct Input {
    pub key: Arc<Key>,
    pub bytes: u64,
    pub token: usize,
}

struct Slot<T, Q> {
    token: usize,
    key: Arc<Key>,
    quality: Q,
    candidate: CandidateKey,
    output: Option<T>,
    ready: bool,
    bytes: u64,
}
struct Slots<T, Q> {
    entries: Vec<Slot<T, Q>>,
    storage: Option<BudgetTicket>,
    next: usize,
}
impl<T, Q> Default for Slots<T, Q> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            storage: None,
            next: 1,
        }
    }
}
pub(crate) struct Cache<T, Q>(Mutex<Slots<T, Q>>);
impl<T, Q> Default for Cache<T, Q> {
    fn default() -> Self {
        Self(Mutex::new(Slots::default()))
    }
}
impl<T: Clone, Q: Copy + PartialEq> Cache<T, Q> {
    #[cfg(feature = "wgpu")]
    pub(crate) fn quality(&self) -> Option<Q> {
        self.0
            .lock()
            .unwrap()
            .entries
            .iter()
            .find(|entry| entry.ready)
            .map(|entry| entry.quality)
    }
    pub(crate) fn lookup(
        &self,
        key: &Arc<Key>,
        quality: Q,
        candidate: &CandidateKey,
    ) -> Option<Input> {
        self.0
            .lock()
            .unwrap()
            .entries
            .iter()
            .find(|entry| {
                entry.ready
                    && entry.quality == quality
                    && (entry.key == *key
                        || entry.candidate == *candidate && entry.key.same_use(key))
            })
            .map(|entry| Input {
                key: entry.key.clone(),
                bytes: entry.bytes,
                token: entry.token,
            })
    }
    pub(crate) fn output(&self, input: &Input) -> T {
        self.0
            .lock()
            .unwrap()
            .entries
            .iter()
            .find(|entry| entry.token == input.token)
            .and_then(|entry| entry.output.as_ref())
            .expect("ready retained input remains active")
            .clone()
    }
    pub(crate) fn retain(&self, plan: &Plan) {
        self.0
            .lock()
            .unwrap()
            .entries
            .retain(|entry| plan.entries.iter().any(|used| used.key == entry.key));
    }
    pub(crate) fn start(
        &self,
        key: Arc<Key>,
        quality: Q,
        candidate: CandidateKey,
        pool: &BudgetPool,
        limit: u64,
    ) -> Result<usize, GpuError> {
        let mut slots = self.0.lock().unwrap();
        if let Some(index) = slots
            .entries
            .iter()
            .position(|entry| entry.key == key && entry.quality == quality)
        {
            let slot = &slots.entries[index];
            if !slot.ready && slot.output.is_some() {
                return Err(if slot.candidate == candidate {
                    GpuError::Preparing
                } else {
                    GpuError::Busy
                });
            }
            if !slot.ready && slot.candidate == candidate {
                return Ok(slot.token);
            }
            slots.entries.remove(index);
        }
        if slots.entries.len() == slots.entries.capacity() {
            let old = slots.entries.capacity() * size_of::<Slot<T, Q>>();
            let capacity = slots.entries.capacity().max(1) * 2;
            let bytes = capacity * size_of::<Slot<T, Q>>();
            if slots.storage.is_none() {
                slots.storage = Some(pool.reserve(0).map_err(|_| GpuError::Pressure {
                    domain: "retained cache",
                    requested: bytes as u64,
                    available: limit.saturating_sub(pool.usage().bytes),
                })?);
            }
            slots
                .storage
                .as_mut()
                .unwrap()
                .resize((old + bytes) as u64)
                .map_err(|_| GpuError::Pressure {
                    domain: "retained cache",
                    requested: bytes as u64,
                    available: limit.saturating_sub(pool.usage().bytes),
                })?;
            let additional = capacity - slots.entries.len();
            slots.entries.reserve_exact(additional);
            let bytes = (slots.entries.capacity() * size_of::<Slot<T, Q>>()) as u64;
            slots
                .storage
                .as_mut()
                .unwrap()
                .resize(bytes)
                .expect("releasing temporary metadata overlap");
        }
        let token = slots.next;
        slots.next = token.checked_add(1).ok_or(GpuError::IdentityExhausted)?;
        slots.entries.push(Slot {
            token,
            key,
            quality,
            candidate,
            output: None,
            ready: false,
            bytes: 0,
        });
        Ok(token)
    }
    pub(crate) fn stage(&self, token: usize, output: T, bytes: u64) {
        let mut slots = self.0.lock().unwrap();
        let entry = slots
            .entries
            .iter_mut()
            .find(|entry| entry.token == token)
            .expect("admitted retained output");
        entry.output = Some(output);
        entry.ready = false;
        entry.bytes = bytes;
    }
    pub(crate) fn complete(&self, token: usize) {
        if let Some(entry) = self
            .0
            .lock()
            .unwrap()
            .entries
            .iter_mut()
            .find(|entry| entry.token == token)
        {
            entry.ready = true;
        }
    }
    pub(crate) fn pending(&self, candidate: &CandidateKey) -> bool {
        self.0
            .lock()
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.candidate == *candidate && entry.output.is_some() && !entry.ready)
    }
    pub(crate) fn cancel(&self, candidate: &CandidateKey) {
        self.0
            .lock()
            .unwrap()
            .entries
            .retain(|entry| entry.candidate != *candidate);
    }
    pub(crate) fn clear(&self) {
        let mut slots = self.0.lock().unwrap();
        slots.entries = Vec::new();
        slots.storage = None;
    }
}
pub(crate) struct Entry {
    pub parent: usize,
    transition: usize,
    pub key: Arc<Key>,
    pub input: Option<Input>,
}

#[derive(Default)]
pub(crate) struct Plan {
    enabled: bool,
    pub entries: Vec<Entry>,
    spans: Vec<(usize, Range<usize>)>,
    pub warm: Option<usize>,
}
struct Cursor<'a> {
    drawing: &'a FrameDrawing,
    range: Range<usize>,
    next: usize,
    transition: usize,
    context: usize,
    seed: Option<Arc<Key>>,
    finish: Option<usize>,
}
impl Plan {
    /// Select only the prefix and old-side task on a miss. The immutable candidate
    /// resumes after that task's fence, with its completed output as an input.
    #[expect(
        clippy::too_many_arguments,
        reason = "one candidate dependency graph and its bounded cache admission"
    )]
    pub(crate) fn collect<'a>(
        frames: impl Iterator<Item = &'a RenderFrame> + Clone,
        size: [u32; 2],
        capture: bool,
        budgets: &Budgets,
        local: &Budgets,
        limit: u64,
        mut lookup: impl FnMut(&Arc<Key>) -> Option<Input>,
        mut allocate: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Self, GpuError> {
        if capture
            || frames
                .clone()
                .all(|frame| frame.drawing().transitions().is_empty())
        {
            return Ok(Self::default());
        }
        let mut plan = Self {
            enabled: true,
            ..Default::default()
        };
        for frame in frames {
            if plan.walk(
                frame.drawing(),
                0..frame.commands().len(),
                0,
                None,
                (size, frame.info().logical_size),
                budgets,
                local,
                limit,
                &mut lookup,
                &mut allocate,
            )? {
                break;
            }
        }
        Ok(plan)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one retained dependency traversal and its admission"
    )]
    fn walk(
        &mut self,
        drawing: &FrameDrawing,
        range: Range<usize>,
        context: usize,
        seed: Option<Arc<Key>>,
        domain: ([u32; 2], [f64; 2]),
        budgets: &Budgets,
        local: &Budgets,
        limit: u64,
        lookup: &mut impl FnMut(&Arc<Key>) -> Option<Input>,
        allocate: &mut impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<bool, GpuError> {
        let mut stack = Vec::new();
        push_stack(
            &mut stack,
            Cursor {
                drawing,
                next: range.start,
                transition: drawing
                    .transitions()
                    .partition_point(|t| t.commands.start < range.start),
                range,
                context,
                seed,
                finish: None,
            },
            &mut *allocate,
        )?;
        while let Some(cursor) = stack.last_mut() {
            let drawing = cursor.drawing;
            let Some(transition) = drawing
                .transitions()
                .get(cursor.transition)
                .filter(|transition| transition.commands.end <= cursor.range.end)
            else {
                self.span(drawing, cursor.next..cursor.range.end, allocate)?;
                if let Some(index) = stack.pop().unwrap().finish {
                    self.warm = Some(index);
                    return Ok(true);
                }
                continue;
            };
            cursor.transition += 1;
            self.span(drawing, cursor.next..transition.commands.start, allocate)?;
            cursor.next = transition.commands.end;
            let key = Key::new(
                drawing,
                transition,
                cursor.seed.clone(),
                domain,
                budgets,
                local,
                limit,
            )?;
            let input = lookup(&key);
            reel_core::profile::retained_cache(input.is_some(), key.bytes.len() as u64);
            let key = input.as_ref().map_or(key, |input| input.key.clone());
            let index = self.entries.len();
            push_stack(
                &mut self.entries,
                Entry {
                    parent: cursor.context,
                    transition: transition as *const _ as usize,
                    key: key.clone(),
                    input,
                },
                &mut *allocate,
            )?;
            if self.entries[index].input.is_none() {
                let old = transition.previous.drawing();
                push_stack(
                    &mut stack,
                    Cursor {
                        drawing: old,
                        range: 0..old.commands().len(),
                        next: 0,
                        transition: 0,
                        context: index + 1,
                        seed: Some(key),
                        finish: Some(index),
                    },
                    &mut *allocate,
                )?;
                continue;
            }
            self.span(drawing, transition.commands.clone(), allocate)?;
        }
        Ok(false)
    }
    fn span(
        &mut self,
        drawing: &FrameDrawing,
        range: Range<usize>,
        allocate: &mut impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<(), GpuError> {
        if !range.is_empty() {
            push_stack(
                &mut self.spans,
                (drawing as *const _ as usize, range),
                allocate,
            )?;
        }
        Ok(())
    }
    pub(crate) fn entry(
        &self,
        context: usize,
        transition: &SceneTransition,
    ) -> Option<(usize, &Entry)> {
        self.entries.iter().enumerate().find(|(_, entry)| {
            entry.parent == context && entry.transition == transition as *const _ as usize
        })
    }
    pub(crate) fn old_needed(&self, transition: &SceneTransition) -> bool {
        !self.enabled
            || self.entries.iter().any(|entry| {
                entry.transition == transition as *const _ as usize && entry.input.is_none()
            })
    }
    pub(crate) fn transition_needed(&self, transition: &SceneTransition) -> bool {
        !self.enabled
            || self
                .entries
                .iter()
                .any(|entry| entry.transition == transition as *const _ as usize)
    }
    #[cfg(any(test, feature = "wgpu"))]
    pub(crate) fn has_cached(&self) -> bool {
        self.entries.iter().any(|entry| entry.input.is_some())
    }
    pub(crate) fn selected(&self, drawing: &FrameDrawing, command: usize) -> bool {
        !self.enabled
            || self.spans.iter().any(|(identity, range)| {
                *identity == drawing as *const _ as usize && range.contains(&command)
            })
    }
    pub(crate) fn persistent_bytes(&self) -> u64 {
        self.entries
            .iter()
            .filter_map(|entry| entry.input.as_ref())
            .enumerate()
            .filter(|(index, input)| {
                !self
                    .entries
                    .iter()
                    .filter_map(|entry| entry.input.as_ref())
                    .take(*index)
                    .any(|earlier| earlier.token == input.token)
            })
            .map(|(_, input)| input.bytes)
            .sum()
    }
}
