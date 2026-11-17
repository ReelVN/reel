//! Stable effect dependencies and optional materialized output pixels.
use reel_core::budget::{BudgetPool, BudgetTicket};
mod admission;
pub(crate) mod chain;
pub(crate) mod context;
pub(crate) mod coverage;
#[cfg(all(feature = "profiling", any(target_os = "vita", feature = "wgpu", test)))]
mod diagnostics;
mod geometry;
mod multiple;
#[cfg(any(feature = "gxm", test))]
mod warming;
pub(crate) use admission::Admission;
#[cfg(test)]
mod content_tests;
#[cfg(test)]
mod tests;
use reel_ui::{
    frame::{FrameCommand, FrameDraw, FrameDrawing, FrameEffect, FrameImage, FrameOp, LocalId},
    imaging::record::Scene,
    kurbo::{Affine, Rect},
    peniko::{Brush, Extend, ImageBrush, ImageQuality},
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
    pub effect_context: Option<context::Key>,
    pub source_size: [u32; 2],
    pub source_storage: [u32; 2],
    /// Validly drawn source pixels; the rest of its storage remains transparent.
    pub source_scissor: [u32; 4],
    pub output_size: [u32; 2],
    pub output_storage: [u32; 2],
    pub scissor: [u32; 4],
    pub quality: [u32; 2],
}
impl Grid {
    pub(crate) fn transforms(source: Affine, effect: Affine) -> ([u64; 6], [u64; 6]) {
        (affine(source), affine(effect))
    }
    /// Use the exact uploaded inputs for an ordinary materialized color pass.
    /// Cropped sources and scalar or terminal passes retain their affine key.
    pub(crate) fn color_context(mut self, effect: &FrameEffect) -> Self {
        self.effect_context = context::ordinary(
            effect,
            Affine::new(self.effect_transform.map(f64::from_bits)),
            self.output_size,
            self.source_size,
            self.source_storage,
        );
        if self.effect_context.is_some() {
            self.effect_transform = affine(Affine::IDENTITY);
        }
        self
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
    appearance: Option<[[u32; 4]; 10]>,
}
impl ImageProjection {
    fn new(
        brush: &ImageBrush<FrameImage>,
        transform: Affine,
        brush_transform: Option<Affine>,
        outer: Affine,
    ) -> Self {
        Self {
            resource: brush.image.resource,
            color: brush.image.color.map(f32::to_bits),
            source_rect: brush.image.source_rect.map(rect),
            x_extend: brush.sampler.x_extend,
            y_extend: brush.sampler.y_extend,
            quality: brush.sampler.quality,
            alpha: brush.sampler.alpha.to_bits(),
            // Replay uses this exact product for the source geometry. Separate
            // ancestor and image transforms can change while their product stays fixed.
            transform: affine(outer * transform),
            brush_transform: brush_transform.map(affine),
            appearance: brush.image.appearance.map(|appearance| {
                super::appearance::appearance_uniform(appearance.transformed(outer))
                    .map(|row| row.map(f32::to_bits))
            }),
        }
    }
}
#[derive(Clone)]
struct Recording {
    owner: u64,
    scene: Arc<Scene>,
    local: LocalId,
    image: ImageProjection,
    geometry: Option<geometry::Key>,
    /// Source-local wrapper bounds, before intersecting the current work ROI.
    bounds: super::scissor::Scissor,
}
impl Recording {
    fn same_pixels(&self, other: &Self) -> bool {
        if self.image == other.image {
            return true;
        }
        let (Some(left), Some(right)) = (&self.geometry, &other.geometry) else {
            return false;
        };
        let mut image = self.image;
        image.transform = other.image.transform;
        image == other.image && left == right
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// The first component identifies a complete pixel dependency set; identical
// computations retain it independently of physical pixel residency. The second
// identifies the stable producer family across changed inputs.
pub(crate) struct Token(pub(crate) usize, pub(crate) usize);
#[allow(
    clippy::large_enum_variant,
    reason = "cache probes keep recording identity inline to avoid a heap allocation on every lookup"
)]
#[derive(Clone)]
enum Source<'a> {
    Recording(Recording),
    Output(Token),
    Multiple(multiple::Images<'a>),
}
impl Source<'_> {
    fn anchor(&self) -> (u8, u64) {
        match self {
            Self::Recording(recording) => (0, recording.owner),
            Self::Output(token) => (1, token.1 as u64),
            Self::Multiple(images) => (0, images.owner()),
        }
    }
    fn same_family(&self, other: &Source<'_>) -> bool {
        match (self, other) {
            (Source::Recording(a), Source::Recording(b)) => a.owner == b.owner,
            (Source::Output(a), Source::Output(b)) => a.1 == b.1,
            (Source::Multiple(a), Source::Multiple(b)) => a.owner() == b.owner(),
            (Source::Recording(a), Source::Multiple(b)) => a.owner == b.owner(),
            (Source::Multiple(a), Source::Recording(b)) => a.owner() == b.owner,
            _ => false,
        }
    }
    fn same(&self, other: &Source<'_>) -> bool {
        match (self, other) {
            (Source::Recording(a), Source::Recording(b)) => {
                a.owner == b.owner
                    && Arc::ptr_eq(&a.scene, &b.scene)
                    && a.local == b.local
                    && a.same_pixels(b)
            }
            (Source::Output(a), Source::Output(b)) => a == b,
            (Source::Multiple(a), Source::Multiple(b)) => a.same(b),
            _ => false,
        }
    }
    fn into_owned(self, pool: &BudgetPool) -> Option<Source<'static>> {
        Some(match self {
            Self::Recording(recording) => Source::Recording(recording),
            Self::Output(token) => Source::Output(token),
            Self::Multiple(images) => Source::Multiple(images.into_owned(pool)?),
        })
    }
}
impl PartialEq for Source<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.same(other)
    }
}
impl Eq for Source<'_> {}

/// Borrowed probe. No serialized command or copied parameter/texture vectors are
/// built on a hit. The scene is upgraded to prevent identity reuse during lookup.
pub(crate) struct Probe<'a> {
    source: Source<'a>,
    effect: &'a FrameEffect,
    grid: Grid,
    /// Present only on a proved exclusive source-chain query.
    required: Option<super::scissor::Scissor>,
}

/// Recorded source coordinates and its proved early cache-reuse boundary.
pub(crate) struct SourceCommands {
    pub commands: Range<usize>,
    pub transform: Affine,
    pub region: super::scissor::Scissor,
    pub early_reusable: bool,
}

/// End of the complete group, including neutral wrappers around its image.
pub(crate) fn source_end(commands: &[FrameCommand], push: usize) -> Option<usize> {
    let mut depth = 0;
    for (index, command) in commands.iter().enumerate().skip(push) {
        match command.op {
            FrameOp::PushGroup(_) => depth += 1,
            FrameOp::PopGroup => {
                depth -= 1;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
    }
    None
}
impl<'a> Probe<'a> {
    pub(crate) fn new(
        drawing: &'a FrameDrawing,
        commands: Range<usize>,
        effect: &'a FrameEffect,
        grid: Grid,
        uncropped: bool,
    ) -> Option<Self> {
        if super::scopes::intersects_transition(drawing, commands.clone()) {
            return None;
        }
        Self::single(drawing, commands.clone(), effect, grid, uncropped)
            .or_else(|| multiple::probe(drawing, commands, effect, grid, uncropped))
    }
    fn single(
        drawing: &FrameDrawing,
        commands: Range<usize>,
        effect: &'a FrameEffect,
        mut grid: Grid,
        uncropped: bool,
    ) -> Option<Self> {
        let commands = drawing.commands().get(commands)?;
        let FrameOp::PushGroup(group) = &commands.first()?.op else {
            return None;
        };
        if group.mask.is_some() || !matches!(commands.last()?.op, FrameOp::PopGroup) {
            return None;
        }
        let owner = group.owner.to_raw();
        let outer = Affine::new(grid.source_transform.map(f64::from_bits));
        let mut source = None;
        let mut depth = 1;
        let [x, y, width, height] = grid.source_scissor;
        let mut region = super::scissor::Scissor {
            x,
            y,
            width,
            height,
        };
        let mut source_bounds = super::scissor::Scissor::full(grid.source_size);
        for command in &commands[1..commands.len() - 1] {
            match &command.op {
                FrameOp::PushContext(_) | FrameOp::PopContext => {}
                FrameOp::PushGroup(group)
                    if source.is_none() && super::scopes::source_over(group, false) =>
                {
                    depth += 1;
                    if let Some(bounds) =
                        super::scissor::Scissor::group_bounds(group, outer, grid.source_size, false)
                    {
                        region = region.intersect(bounds);
                        source_bounds = source_bounds.intersect(bounds);
                    }
                }
                FrameOp::PopGroup if source.is_some() && depth > 1 => depth -= 1,
                FrameOp::Draw(
                    draw @ FrameDraw::Fill {
                        transform,
                        brush_transform,
                        brush: Brush::Image(brush),
                        composite,
                        ..
                    },
                ) if source.is_none() && (depth == 1 || composite.blend == Default::default()) => {
                    let (recording, local) = drawing.recording(command.origin)?;
                    if !recording.mask_path().is_empty() {
                        return None;
                    }
                    source = Some(Source::Recording(Recording {
                        owner,
                        scene: recording.root().upgrade()?,
                        local,
                        image: ImageProjection::new(brush, *transform, *brush_transform, outer),
                        geometry: uncropped
                            .then(|| geometry::image(drawing, draw, outer, grid.source_size))
                            .flatten(),
                        bounds: source_bounds,
                    }));
                }
                _ => return None,
            }
        }
        if depth != 1 {
            return None;
        }
        grid.source_scissor = [region.x, region.y, region.width, region.height];
        // The source matrix has been consumed by the exact geometry and
        // appearance projections above; all physical extents and scissors remain keyed.
        grid.source_transform = affine(Affine::IDENTITY);
        Some(Self {
            source: source?,
            effect,
            grid,
            required: None,
        })
    }
    /// The token identifies the complete inputs of the preceding effect.
    pub(crate) fn from_output(token: Token, effect: &'a FrameEffect, mut grid: Grid) -> Self {
        // The preceding raster grid is already part of the content identity.
        grid.source_transform = affine(Affine::IDENTITY);
        Self {
            source: Source::Output(token),
            effect,
            grid,
            required: None,
        }
    }
}

pub(crate) struct Key {
    source: Source<'static>,
    definition: [u8; 32],
    shader: ResourceHandle,
    values: Vec<EffectValue>,
    images: Vec<ResourceHandle>,
    bounds: [u64; 4],
    coverage_invert: bool,
    resolution_scale: u64,
    grid: Grid,
    _storage: BudgetTicket,
}
impl Key {
    pub(crate) fn matches(&self, probe: &Probe<'_>) -> bool {
        self.grid == probe.grid && self.dependencies_match(probe)
    }
    fn dependencies_match(&self, probe: &Probe<'_>) -> bool {
        self.definition == probe.effect.definition
            && self.shader == probe.effect.shader
            && self.values == probe.effect.values
            && self.images == probe.effect.images
            && self.bounds == rect(probe.effect.bounds)
            && self.coverage_invert == probe.effect.coverage_invert
            && self.resolution_scale == probe.effect.resolution_scale.to_bits()
            && self.source.same(&probe.source)
    }
    fn same_family(&self, other: &Self) -> bool {
        self.source.same_family(&other.source) && self.definition == other.definition
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
            source: probe.source.into_owned(pool)?,
            definition: probe.effect.definition,
            shader: probe.effect.shader,
            values: probe.effect.values.clone(),
            images: probe.effect.images.clone(),
            bounds: rect(probe.effect.bounds),
            coverage_invert: probe.effect.coverage_invert,
            resolution_scale: probe.effect.resolution_scale.to_bits(),
            grid: probe.grid,
            _storage: storage,
        })
    }
}

struct Entry<T> {
    key: Key,
    token: Token,
    output: Option<T>,
    tail_only: bool,
    #[cfg(any(feature = "gxm", test))]
    warming: Option<[u32; 2]>,
    last_used: u64,
}
struct State<T> {
    entries: Vec<Entry<T>>,
    metadata: Option<BudgetTicket>,
    next: usize,
    submitted: u64,
    cursor: usize,
    #[cfg(test)]
    hits: [usize; 2],
}
impl<T> Default for State<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            metadata: None,
            next: 1,
            submitted: 0,
            cursor: 0,
            #[cfg(test)]
            hits: [0; 2],
        }
    }
}
/// Content identity and optional pixels. Equal dependencies retain their token;
/// changed or cleared producers receive a fresh token. Pixel owners keep their
/// original target reservations independently of metadata observations.
pub(crate) struct Cache<T>(std::sync::Mutex<State<T>>);
impl<T> Default for Cache<T> {
    fn default() -> Self {
        Self(std::sync::Mutex::new(State::default()))
    }
}
pub(crate) struct Candidate {
    key: Key,
    pub(crate) token: Token,
    /// New producers may retain immediately; changed producers first publish
    /// metadata until an identical dependency set recurs in a later candidate.
    pub(crate) retain: bool,
    #[cfg(any(feature = "gxm", test))]
    observed: bool,
}
pub(crate) struct Pending<T> {
    key: Key,
    pub(crate) token: Token,
    output: Option<T>,
    replaces_source: bool,
    eligible: bool,
    tail_only: bool,
    #[cfg(any(feature = "gxm", test))]
    warming: Option<[u32; 2]>,
}
impl Candidate {
    pub(crate) fn finish<T>(self, output: T) -> Pending<T> {
        assert!(self.retain, "changed effect must publish observation first");
        Pending {
            key: self.key,
            token: self.token,
            output: Some(output),
            replaces_source: false,
            eligible: self.retain,
            tail_only: false,
            #[cfg(any(feature = "gxm", test))]
            warming: None,
        }
    }
    /// Retain a consumer proved reusable at its source entry, so publishing its
    /// pixels can retire the preceding materialized input without replaying it.
    pub(crate) fn finish_replacing_source<T>(self, output: T) -> Pending<T> {
        let mut pending = self.finish(output);
        pending.replaces_source = true;
        pending
    }
    /// Publish only dependency identity, leaving this frame's pixels recyclable.
    pub(crate) fn observe<T>(self) -> Pending<T> {
        Pending {
            key: self.key,
            token: self.token,
            output: None,
            replaces_source: false,
            eligible: self.retain,
            tail_only: false,
            #[cfg(any(feature = "gxm", test))]
            warming: None,
        }
    }
}
impl<T> Pending<T> {
    /// Attach pixels still carrying this eligible producer's final write. The
    /// caller pins their existing allocation through the producing submission.
    pub(crate) fn materialize(&mut self, output: T, replaces_source: bool) {
        assert!(
            self.eligible && self.output.is_none(),
            "eligible unpublished effect pixels"
        );
        self.output = Some(output);
        self.replaces_source = replaces_source;
        self.tail_only = true;
        #[cfg(any(feature = "gxm", test))]
        {
            self.warming = None;
        }
    }
}
impl<T: Clone> Cache<T> {
    /// A metadata-only cache cannot replace a source in this preparation.
    pub(crate) fn has_outputs(&self) -> bool {
        self.0
            .lock()
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.output.is_some())
    }
    /// Resolve submitted content identity without requiring retained pixels.
    /// Touching an observation preserves any existing materialized output.
    pub(crate) fn lookup_content(&self, probe: &Probe<'_>) -> Option<Token> {
        let mut state = self.0.lock().unwrap();
        let anchor = probe.source.anchor();
        let start = state
            .entries
            .partition_point(|entry| entry.key.source.anchor() < anchor);
        let offset = state.entries[start..]
            .iter()
            .take_while(|entry| entry.key.source.anchor() == anchor)
            .position(|entry| entry.key.matches(probe) || entry.key.covers(probe))?;
        let submitted = state.submitted;
        let entry = &mut state.entries[start + offset];
        entry.last_used = submitted;
        Some(entry.token)
    }

    pub(crate) fn lookup(&self, probe: &Probe<'_>) -> Option<(Token, T)> {
        let mut state = self.0.lock().unwrap();
        let anchor = probe.source.anchor();
        let start = state
            .entries
            .partition_point(|entry| entry.key.source.anchor() < anchor);
        let range = &state.entries[start..];
        let Some(offset) = range
            .iter()
            .take_while(|entry| entry.key.source.anchor() == anchor)
            .position(|entry| entry.key.matches(probe) || entry.key.covers(probe))
        else {
            #[cfg(all(feature = "profiling", any(target_os = "vita", feature = "wgpu", test)))]
            diagnostics::record(range, probe);
            return None;
        };
        let index = start + offset;
        let submitted = state.submitted;
        let entry = &mut state.entries[index];
        entry.last_used = submitted;
        let Some(output) = entry.output.as_ref() else {
            #[cfg(all(feature = "profiling", any(target_os = "vita", feature = "wgpu", test)))]
            diagnostics::record(std::slice::from_ref(entry), probe);
            return None;
        };
        let hit = (entry.token, output.clone());
        #[cfg(test)]
        {
            state.hits[anchor.0 as usize] += 1;
        }
        Some(hit)
    }
    #[cfg(test)]
    pub(crate) fn test_stats(&self) -> (usize, [usize; 2]) {
        let state = self.0.lock().unwrap();
        (
            state
                .entries
                .iter()
                .filter(|entry| entry.output.is_some())
                .count(),
            state.hits,
        )
    }
    /// Preparation is private to a candidate. It cannot be observed by another
    /// candidate until the renderer successfully submits the producing work.
    pub(crate) fn prepare(&self, probe: Probe<'_>, pool: &BudgetPool) -> Option<Candidate> {
        let mut state = self.0.lock().unwrap();
        let anchor = probe.source.anchor();
        let start = state
            .entries
            .partition_point(|entry| entry.key.source.anchor() < anchor);
        let previous = state.entries[start..]
            .iter()
            .take_while(|entry| entry.key.source.anchor() == anchor)
            .find(|entry| {
                entry.key.source.same_family(&probe.source)
                    && entry.key.definition == probe.effect.definition
            });
        let observed = previous.is_some_and(|entry| entry.key.matches(&probe));
        let retain = previous.is_none() || observed;
        let family = previous.map_or(state.next, |entry| entry.token.1);
        let key = Key::new(probe, pool)?;
        let token = if observed {
            previous.unwrap().token
        } else {
            let token = Token(state.next, family);
            state.next = state.next.checked_add(1)?;
            token
        };
        Some(Candidate {
            key,
            token,
            retain,
            #[cfg(any(feature = "gxm", test))]
            observed,
        })
    }
    pub(crate) fn publish(&self, pending: Pending<T>, pool: &BudgetPool) -> bool {
        let consumed = match (
            &pending.key.source,
            &pending.output,
            pending.replaces_source,
        ) {
            (Source::Output(token), Some(_), true) => Some(*token),
            _ => None,
        };
        let mut state = self.0.lock().unwrap();
        let anchor = pending.key.source.anchor();
        let start = state
            .entries
            .partition_point(|entry| entry.key.source.anchor() < anchor);
        let end = start
            + state.entries[start..].partition_point(|entry| entry.key.source.anchor() == anchor);
        if let Some(index) = (start..end).find(|&i| state.entries[i].key.same_family(&pending.key))
        {
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
            if let Some(metadata) = &mut state.metadata {
                if metadata.resize(peak as u64).is_err() {
                    return false;
                }
            } else {
                let Ok(ticket) = pool.reserve(peak as u64) else {
                    return false;
                };
                state.metadata = Some(ticket);
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
        let submitted = state.submitted;
        state.entries.insert(
            index,
            Entry {
                key: pending.key,
                token: pending.token,
                output: pending.output,
                tail_only: pending.tail_only,
                #[cfg(any(feature = "gxm", test))]
                warming: pending.warming,
                last_used: submitted,
            },
        );
        // A retained consumer replaces this prefix's optional pixels. Keep its
        // content identity for chain resolution; submitted readers own clones.
        if let Some(token) = consumed
            && let Some(input) = state.entries.iter_mut().find(|entry| entry.token == token)
        {
            input.output = None;
            #[cfg(any(feature = "gxm", test))]
            {
                input.warming = None;
            }
        }
        true
    }
    /// Advance only after submission and inspect one optional result at a time.
    pub(crate) fn submitted(&self) {
        let mut state = self.0.lock().unwrap();
        state.submitted = state.submitted.saturating_add(1);
        if state.entries.is_empty() {
            state.cursor = 0;
            return;
        }
        let index = state.cursor % state.entries.len();
        if state
            .submitted
            .saturating_sub(state.entries[index].last_used)
            >= u64::from(super::target_cache::MAX_IDLE_SUBMISSIONS)
        {
            state.entries.remove(index);
            if state.entries.is_empty() {
                state.entries = Vec::new();
                state.metadata = None;
                state.cursor = 0;
            } else {
                state.cursor = index % state.entries.len();
            }
        } else {
            state.cursor = (index + 1) % state.entries.len();
        }
    }
    pub(crate) fn clear(&self) {
        let mut state = self.0.lock().unwrap();
        state.entries = Vec::new();
        state.metadata = None;
        state.cursor = 0;
    }
    /// Return optional GPU owners and replacement requests while preserving
    /// submitted content identities and raster evidence. Readers already
    /// admitted by a candidate retain their independent clones.
    #[cfg(any(feature = "wgpu", feature = "gxm", test))]
    pub(crate) fn release_outputs(&self) {
        let mut state = self.0.lock().unwrap();
        for entry in &mut state.entries {
            entry.output = None;
            #[cfg(any(feature = "gxm", test))]
            {
                entry.warming = None;
            }
        }
    }
    /// Return sealed scratch pixels that this preparation did not select as
    /// inputs. Submitted readers retain their own pins and content stays known.
    pub(crate) fn release_tail_outputs(&self, mut keep: impl FnMut(Token) -> bool) -> usize {
        let mut state = self.0.lock().unwrap();
        let mut released = 0;
        for entry in &mut state.entries {
            if entry.tail_only && entry.output.is_some() && !keep(entry.token) {
                entry.output = None;
                #[cfg(any(feature = "gxm", test))]
                {
                    entry.warming = None;
                }
                released += 1;
            }
        }
        #[cfg(feature = "profiling")]
        crate::profiling::record_effect_input_event(
            crate::profiling::EffectInputEvent::TailOutputsReleased,
            released as u64,
        );
        released
    }
    /// Release one optional pixel owner before retrying a required allocation.
    /// Keep its observation and family; submitted readers own their own clones.
    pub(crate) fn reclaim_output(&self) -> bool {
        let mut state = self.0.lock().unwrap();
        let Some(index) = state
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.output.is_some())
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(index, _)| index)
        else {
            return false;
        };
        state.entries[index].output = None;
        #[cfg(any(feature = "gxm", test))]
        {
            state.entries[index].warming = None;
        }
        true
    }
}
