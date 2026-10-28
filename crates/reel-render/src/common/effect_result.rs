#![allow(
    dead_code,
    reason = "effect replay integration is being developed in the next slice"
)]
//! Content identity for already-materialized ordinary effect outputs.
use reel_core::budget::{BudgetPool, BudgetTicket};
#[cfg(test)]
mod tests;
use reel_ui::{
    frame::{FrameDraw, FrameDrawing, FrameEffect, FrameOp, LocalId},
    imaging::record::Scene,
    kurbo::{Affine, Rect},
    peniko::{Brush, Extend, ImageQuality},
    resources::ResourceHandle,
    stage::effect::EffectValue,
};
use std::{ops::Range, sync::Arc};

fn affine(value: Affine) -> [u64; 6] {
    value.as_coeffs().map(f64::to_bits)
}
fn rect(value: Rect) -> [u64; 4] {
    [
        value.x0.to_bits(),
        value.y0.to_bits(),
        value.x1.to_bits(),
        value.y1.to_bits(),
    ]
}

/// Exact raster grid. Values must describe the actual effect invocation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Grid {
    pub source_transform: [u64; 6],
    pub effect_transform: [u64; 6],
    pub source_size: [u32; 2],
    pub source_storage: [u32; 2],
    pub output_size: [u32; 2],
    pub output_storage: [u32; 2],
    pub scissor: [u32; 4],
    pub quality: [u32; 2],
}
impl Grid {
    pub(crate) fn transforms(source: Affine, effect: Affine) -> ([u64; 6], [u64; 6]) {
        (affine(source), affine(effect))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ImageProjection {
    resource: ResourceHandle,
    color: [u32; 4],
    source_rect: Option<[u64; 4]>,
    x_extend: Extend,
    y_extend: Extend,
    quality: ImageQuality,
    alpha: u32,
    transform: [u64; 6],
    brush_transform: Option<[u64; 6]>,
}
struct Recording {
    scene: Arc<Scene>,
    local: LocalId,
    image: ImageProjection,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Token(pub(crate) usize);
enum Source {
    Recording(Recording),
    Output(Token),
}
impl Source {
    fn anchor(&self) -> (u8, usize) {
        match self {
            Self::Recording(recording) => (0, Arc::as_ptr(&recording.scene) as usize),
            Self::Output(token) => (1, token.0),
        }
    }
}
impl PartialEq for Source {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Recording(a), Self::Recording(b)) => {
                Arc::ptr_eq(&a.scene, &b.scene) && a.local == b.local && a.image == b.image
            }
            (Self::Output(a), Self::Output(b)) => a == b,
            _ => false,
        }
    }
}
impl Eq for Source {}

/// Borrowed probe. No serialized command or copied parameter/texture vectors are
/// built on a hit. The scene is upgraded to prevent identity reuse during lookup.
pub(crate) struct Probe<'a> {
    source: Source,
    effect: &'a FrameEffect,
    grid: Grid,
}
impl<'a> Probe<'a> {
    pub(crate) fn new(
        drawing: &FrameDrawing,
        commands: Range<usize>,
        effect: &'a FrameEffect,
        grid: Grid,
    ) -> Option<Self> {
        if !drawing.transitions().is_empty() || !drawing.masks().is_empty() {
            return None;
        }
        let commands = drawing.commands().get(commands)?;
        if !matches!(commands.first()?.op, FrameOp::PushGroup(_))
            || !matches!(commands.last()?.op, FrameOp::PopGroup)
        {
            return None;
        }
        let mut source = None;
        for command in &commands[1..commands.len() - 1] {
            match &command.op {
                FrameOp::PushContext(_) | FrameOp::PopContext => {}
                FrameOp::Draw(FrameDraw::Fill {
                    transform,
                    brush_transform,
                    brush: Brush::Image(brush),
                    ..
                }) if source.is_none() => {
                    if brush.image.appearance.is_some() {
                        return None;
                    }
                    let (recording, local) = drawing.recording(command.origin)?;
                    if !recording.mask_path().is_empty() {
                        return None;
                    }
                    source = Some(Source::Recording(Recording {
                        scene: recording.root().upgrade()?,
                        local,
                        image: ImageProjection {
                            resource: brush.image.resource,
                            color: brush.image.color.map(f32::to_bits),
                            source_rect: brush.image.source_rect.map(rect),
                            x_extend: brush.sampler.x_extend,
                            y_extend: brush.sampler.y_extend,
                            quality: brush.sampler.quality,
                            alpha: brush.sampler.alpha.to_bits(),
                            transform: affine(*transform),
                            brush_transform: brush_transform.map(affine),
                        },
                    }));
                }
                _ => return None,
            }
        }
        Some(Self {
            source: source?,
            effect,
            grid,
        })
    }
    /// The token denotes an immutable, exclusively held previous effect result.
    pub(crate) fn from_output(token: Token, effect: &'a FrameEffect, grid: Grid) -> Self {
        Self {
            source: Source::Output(token),
            effect,
            grid,
        }
    }
}

pub(crate) struct Key {
    source: Source,
    definition: [u8; 32],
    shader: ResourceHandle,
    values: Vec<EffectValue>,
    images: Vec<ResourceHandle>,
    bounds: [u64; 4],
    coverage_invert: bool,
    grid: Grid,
    _storage: BudgetTicket,
}
impl Key {
    pub(crate) fn matches(&self, probe: &Probe<'_>) -> bool {
        self.source == probe.source
            && self.definition == probe.effect.definition
            && self.shader == probe.effect.shader
            && self.values == probe.effect.values
            && self.images == probe.effect.images
            && self.bounds == rect(probe.effect.bounds)
            && self.coverage_invert == probe.effect.coverage_invert
            && self.grid == probe.grid
    }
    fn same_key(&self, other: &Self) -> bool {
        self.source == other.source
            && self.definition == other.definition
            && self.shader == other.shader
            && self.values == other.values
            && self.images == other.images
            && self.bounds == other.bounds
            && self.coverage_invert == other.coverage_invert
            && self.grid == other.grid
    }
    /// Optional cache admission: failure leaves the ordinary render path intact.
    pub(crate) fn new(probe: Probe<'_>, pool: &BudgetPool) -> Option<Self> {
        let bytes = size_of::<Self>()
            .checked_add(
                probe
                    .effect
                    .values
                    .len()
                    .checked_mul(size_of::<EffectValue>())?,
            )?
            .checked_add(
                probe
                    .effect
                    .images
                    .len()
                    .checked_mul(size_of::<ResourceHandle>())?,
            )?;
        let storage = pool.reserve(bytes as u64).ok()?;
        Some(Self {
            source: probe.source,
            definition: probe.effect.definition,
            shader: probe.effect.shader,
            values: probe.effect.values.clone(),
            images: probe.effect.images.clone(),
            bounds: rect(probe.effect.bounds),
            coverage_invert: probe.effect.coverage_invert,
            grid: probe.grid,
            _storage: storage,
        })
    }
}

struct Entry<T> {
    key: Key,
    token: Token,
    output: T,
    age: u8,
}
struct State<T> {
    entries: Vec<Entry<T>>,
    metadata: Option<BudgetTicket>,
    next: usize,
}
impl<T> Default for State<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            metadata: None,
            next: 1,
        }
    }
}
/// Optional results; output owners retain their original target reservations.
/// Tokens never repeat within this renderer, including after eviction or clear.
pub(crate) struct Cache<T>(std::sync::Mutex<State<T>>);
impl<T> Default for Cache<T> {
    fn default() -> Self {
        Self(std::sync::Mutex::new(State::default()))
    }
}
pub(crate) struct Candidate {
    key: Key,
    pub(crate) token: Token,
}
pub(crate) struct Pending<T> {
    key: Key,
    pub(crate) token: Token,
    pub(crate) output: T,
}
impl Candidate {
    pub(crate) fn finish<T>(self, output: T) -> Pending<T> {
        Pending {
            key: self.key,
            token: self.token,
            output,
        }
    }
}
impl<T: Clone> Cache<T> {
    pub(crate) fn lookup(&self, probe: &Probe<'_>) -> Option<(Token, T)> {
        let mut state = self.0.lock().unwrap();
        let anchor = probe.source.anchor();
        let start = state
            .entries
            .partition_point(|entry| entry.key.source.anchor() < anchor);
        let entry = state.entries[start..]
            .iter_mut()
            .take_while(|entry| entry.key.source.anchor() == anchor)
            .find(|entry| entry.key.matches(probe))?;
        entry.age = 0;
        Some((entry.token, entry.output.clone()))
    }
    /// Preparation is private to a candidate. It cannot be observed by another
    /// candidate until the renderer successfully submits the producing work.
    pub(crate) fn prepare(&self, probe: Probe<'_>, pool: &BudgetPool) -> Option<Candidate> {
        let key = Key::new(probe, pool)?;
        let mut state = self.0.lock().unwrap();
        let token = Token(state.next);
        state.next = state.next.checked_add(1)?;
        Some(Candidate { key, token })
    }
    pub(crate) fn publish(&self, pending: Pending<T>, pool: &BudgetPool) -> bool {
        let mut state = self.0.lock().unwrap();
        let anchor = pending.key.source.anchor();
        let start = state
            .entries
            .partition_point(|entry| entry.key.source.anchor() < anchor);
        let end = start
            + state.entries[start..].partition_point(|entry| entry.key.source.anchor() == anchor);
        if let Some(index) = (start..end).find(|&i| state.entries[i].key.same_key(&pending.key)) {
            state.entries.remove(index);
        }
        if state.entries.len() == state.entries.capacity() {
            let old = state.entries.capacity();
            let Some(capacity) = old.max(1).checked_mul(2) else {
                return false;
            };
            let Some(peak) = old
                .checked_add(capacity)
                .and_then(|v| v.checked_mul(size_of::<Entry<T>>()))
            else {
                return false;
            };
            if state.metadata.is_none() {
                let Ok(ticket) = pool.reserve(0) else {
                    return false;
                };
                state.metadata = Some(ticket);
            }
            if state
                .metadata
                .as_mut()
                .unwrap()
                .resize(peak as u64)
                .is_err()
            {
                return false;
            }
            let additional = capacity - state.entries.len();
            state.entries.reserve_exact(additional);
            let bytes = state.entries.capacity() * size_of::<Entry<T>>();
            state
                .metadata
                .as_mut()
                .unwrap()
                .resize(bytes as u64)
                .expect("admitted cache metadata");
        }
        let index = state
            .entries
            .partition_point(|entry| entry.key.source.anchor() <= anchor);
        state.entries.insert(
            index,
            Entry {
                key: pending.key,
                token: pending.token,
                output: pending.output,
                age: 0,
            },
        );
        true
    }
    /// Only accepted submissions age cached outputs.
    pub(crate) fn submitted(&self) {
        let mut state = self.0.lock().unwrap();
        for entry in &mut state.entries {
            entry.age = entry.age.saturating_add(1);
        }
    }
    pub(crate) fn reclaim_one(&self) -> bool {
        let mut state = self.0.lock().unwrap();
        let Some((index, _)) = state
            .entries
            .iter()
            .enumerate()
            .max_by_key(|(_, entry)| entry.age)
        else {
            return false;
        };
        state.entries.remove(index);
        if state.entries.is_empty() {
            state.entries = Vec::new();
            state.metadata = None;
        }
        true
    }
    pub(crate) fn clear(&self) {
        let mut state = self.0.lock().unwrap();
        state.entries = Vec::new();
        state.metadata = None;
    }
}
