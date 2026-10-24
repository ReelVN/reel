// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! R01 semantic frame compiler. No backend, native handles, tessellation or shaping.
use crate::bridge::{CandidateGuard, CandidateKey, RecordedSource};
use crate::resources::{ResourceHandle, ResourceSnapshot};
use crate::{
    app::{GroupMetadata, PresentedGeometryView, VisualLayerKind},
    core::{PaintImageMask, WidgetId},
    imaging::{
        self, Composite, Filter, MaskMode,
        record::{self, Clip, Command, Draw, Geometry, Scene},
    },
    kurbo::{Affine, BezPath, PathEl, Rect, Stroke, Vec2},
    peniko::{self, Brush, Gradient, ImageBrush, Style},
};
use reel_core::{
    budget::{BudgetPool, BudgetTicket},
    identity::{FrameId, SurfaceEpoch},
};
use std::{collections::BTreeMap, mem::size_of, ops::Range, sync::Arc};

mod archive;
mod blend;
pub use blend::{BlendMode, GroupComposite};
mod retained;
mod wire;
pub use archive::{
    RestoreProgress, RetainedAsset, RetainedAssetKind, RetainedRestore, SavedRetainedScene,
};
pub use retained::{FrameTransition, RetainBuild, RetainError, RetainedScene, SceneTransition};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameInfo {
    pub logical_size: [f64; 2],
    pub physical_size: [u32; 2],
}
#[derive(Clone, Copy, Debug)]
pub struct BuildLimits {
    pub max_bytes: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct StepBudget {
    pub max_commands: u32,
    pub max_units: u64,
    pub max_bytes: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BuildProgress {
    pub complete: bool,
    pub commands: u32,
    pub units: u64,
    pub bytes: u64,
    pub minimum_units: u64,
    pub minimum_bytes: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordLocation {
    pub fragment: usize,
    pub command: usize,
    pub mask_depth: usize,
    pub owner: WidgetId,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildErrorKind {
    Capacity,
    Bytes,
    InvalidRecord,
    UnsupportedRecord,
    MissingGroupMetadata,
    MissingResource { blob_id: u64 },
    MissingExternal { token: u64 },
    Cancelled,
    Incomplete,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildError {
    pub at: RecordLocation,
    pub kind: BuildErrorKind,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalId {
    Clip(record::ClipId),
    Group(record::GroupId),
    Draw(record::DrawId),
    Context(record::ContextId),
    None,
}
/// Scene IDs are local. The pair (source,local) is a frame mapping, never local alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandOrigin {
    pub source: u32,
    pub local: LocalId,
    pub at: RecordLocation,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FrameCommand {
    // Retained drawings have no source recording or live widget identity to restore.
    #[serde(skip, default = "wire::origin")]
    pub origin: CommandOrigin,
    pub op: FrameOp,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum FrameOp {
    PushClip(#[serde(with = "wire::ClipDef")] Clip),
    PopClip,
    PushGroup(FrameGroup),
    PopGroup,
    PushContext(FrameContext),
    PopContext,
    Draw(FrameDraw),
    External(ExternalDraw),
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExternalDraw {
    pub resource: ResourceHandle,
    pub token: u64,
    #[serde(with = "wire::widget")]
    pub owner: WidgetId,
    pub local_bounds: Rect,
    pub content_to_window: Affine,
}
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MaskUse {
    pub mask: u32,
    pub transform: Affine,
}
/// Explicitly conservative. Unknown glyph outlines and flood effects remain unbounded;
/// a backend clamps these to its output target rather than trusting a layout rectangle.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum InkBounds {
    Empty,
    Rect(Rect),
    Unbounded,
}
impl InkBounds {
    fn union(self, other: Self) -> Self {
        match (self, other) {
            (Self::Unbounded, _) | (_, Self::Unbounded) => Self::Unbounded,
            (Self::Empty, b) => b,
            (a, Self::Empty) => a,
            (Self::Rect(a), Self::Rect(b)) => Self::Rect(a.union(b)),
        }
    }
    fn transformed(self, t: Affine) -> Self {
        match self {
            Self::Rect(r) => {
                let bounds = t.transform_rect_bbox(r);
                if bounds.is_finite() {
                    Self::Rect(bounds)
                } else {
                    Self::Unbounded
                }
            }
            other => other,
        }
    }
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FrameImageMask {
    #[serde(with = "wire::mask_program")]
    pub program: Arc<crate::stage::MaskProgram>,
    pub images: Vec<ResourceHandle>,
    pub bounds: Rect,
    pub parameters: [f32; 5],
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FrameGroup {
    pub image_mask: Option<FrameImageMask>,
    #[serde(with = "wire::optional_clip")]
    pub clip: Option<Clip>,
    pub mask: Option<MaskUse>,
    #[serde(with = "wire::filters")]
    pub filters: Vec<Filter>,
    pub composite: GroupComposite,
    pub invert: f32,
    #[serde(with = "wire::widget")]
    pub owner: WidgetId,
    pub effect_to_target: Affine,
    pub ink_bounds: InkBounds,
}
/// One use of shared image pixels. Uniform tint travels in draw vertices;
/// appearance uses the same texture without an offscreen composition.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FrameImage {
    pub resource: ResourceHandle,
    /// Linear premultiplied RGBA multiplier, including its own alpha once.
    pub color: [f32; 4],
    /// Optional corner tint, sRGB adjustment and packed alpha layout.
    /// None uses the shared plain uniform.
    pub appearance: Option<ImageAppearance>,
    /// Optional source-pixel sampling window. Per-fragment padding preserves the
    /// original UV mapping while preventing bilinear reads from adjacent regions.
    pub source_rect: Option<Rect>,
}
mod image_color;
pub use image_color::{ImageAlphaLayout, ImageAppearance, ImageColorAdjustment};
/// Four linear premultiplied colors and their unit square in frame coordinates.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImageColorGradient {
    pub colors: [[f32; 4]; 4],
    pub transform: Affine,
}
impl ImageColorGradient {
    pub fn transformed(mut self, transform: Affine) -> Self {
        self.transform = transform * self.transform;
        self
    }
}
pub type FrameBrush = Brush<ImageBrush<FrameImage>, Gradient>;
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum FrameDraw {
    Fill {
        transform: Affine,
        fill_rule: peniko::Fill,
        brush: FrameBrush,
        /// For images: local-to-source pixel sampling, permitting zero scale.
        brush_transform: Option<Affine>,
        #[serde(with = "wire::GeometryDef")]
        shape: Geometry,
        #[serde(with = "wire::CompositeDef")]
        composite: Composite,
    },
    Stroke {
        transform: Affine,
        stroke: Stroke,
        brush: FrameBrush,
        /// For images: local-to-source pixel sampling, permitting zero scale.
        brush_transform: Option<Affine>,
        #[serde(with = "wire::GeometryDef")]
        shape: Geometry,
        #[serde(with = "wire::CompositeDef")]
        composite: Composite,
    },
    GlyphRun(FrameGlyphRun),
    BlurredRoundedRect(#[serde(with = "wire::BlurredRectDef")] imaging::BlurredRoundedRect),
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FrameGlyphRun {
    pub font: ResourceHandle,
    pub font_index: u32,
    pub transform: Affine,
    pub glyph_transform: Option<Affine>,
    pub brush_transform: Option<Affine>,
    pub font_size: f32,
    /// Total synthetic increase in stroke thickness along each logical axis.
    /// Rasterizers convert this to per-side outline offsets after density scaling.
    pub font_embolden: Vec2,
    pub hint: bool,
    pub normalized_coords: Vec<imaging::NormalizedCoord>,
    pub style: Style,
    #[serde(with = "wire::glyphs")]
    pub glyphs: Vec<record::Glyph>,
    pub brush: FrameBrush,
    #[serde(with = "wire::CompositeDef")]
    pub composite: Composite,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct OwnedText(Vec<u8>);
impl OwnedText {
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("compiler copied complete UTF-8 source")
    }
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum FrameContextKind {
    Label,
    Widget,
    ChildIndex,
    Slot,
    Named(OwnedText),
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum FrameContextValue {
    Str(OwnedText),
    U64(u64),
    I64(i64),
    Usize(usize),
    Bool(bool),
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FrameSourceLocation {
    pub file: OwnedText,
    pub line: u32,
    pub column: u32,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FrameContext {
    pub kind: FrameContextKind,
    pub value: FrameContextValue,
    pub source: Option<FrameSourceLocation>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FrameMask {
    #[serde(with = "wire::MaskModeDef")]
    pub mode: MaskMode,
    pub commands: Vec<FrameCommand>,
}
#[derive(Clone, Debug)]
pub struct RenderFrame {
    inner: Arc<FrameData>,
}
#[derive(Debug)]
struct FrameData {
    _guard: CandidateGuard,
    key: CandidateKey,
    info: FrameInfo,
    geometry: Arc<PresentedGeometryView>,
    drawing: Arc<FrameDrawing>,
    subtrees: Vec<FrameSubtree>,
    transition_roots: Vec<Option<WidgetId>>,
    body_end: usize,
}
/// Immutable drawing and resource ownership, independent of frame acceptance and input.
#[derive(Debug)]
pub struct FrameDrawing {
    commands: Vec<FrameCommand>,
    capture_exclusions: Vec<Range<usize>>,
    masks: BTreeMap<u32, Arc<FrameMask>>,
    resources: ResourceSnapshot,
    transitions: Vec<SceneTransition>,
    accounted_bytes: u64,
    _ticket: BudgetTicket,
}
type FrameSubtree = crate::app::SubtreePaint;
impl FrameDrawing {
    /// Sorted balanced command intervals omitted only during capture replay.
    pub fn capture_exclusions(&self) -> &[Range<usize>] {
        &self.capture_exclusions
    }
    /// Return the next retained command index when this command is excluded.
    pub fn capture_exclusion_end(&self, index: usize) -> Option<usize> {
        let slot = self
            .capture_exclusions
            .partition_point(|range| range.end <= index);
        self.capture_exclusions
            .get(slot)
            .filter(|range| range.start <= index)
            .map(|range| range.end)
    }
    pub fn commands(&self) -> &[FrameCommand] {
        &self.commands
    }
    pub fn masks(&self) -> &BTreeMap<u32, Arc<FrameMask>> {
        &self.masks
    }
    pub fn resources(&self) -> &ResourceSnapshot {
        &self.resources
    }
    pub fn transitions(&self) -> &[SceneTransition] {
        &self.transitions
    }
    pub fn accounted_bytes(&self) -> u64 {
        self.accounted_bytes
    }
}
impl RenderFrame {
    pub fn drawing(&self) -> &FrameDrawing {
        &self.inner.drawing
    }
    pub fn key(&self) -> CandidateKey {
        self.inner.key.clone()
    }
    pub fn frame_id(&self) -> FrameId {
        self.inner.key.frame_id()
    }
    pub fn surface_epoch(&self) -> SurfaceEpoch {
        self.inner.key.surface_epoch()
    }
    pub fn info(&self) -> FrameInfo {
        self.inner.info
    }
    pub fn geometry(&self) -> &PresentedGeometryView {
        &self.inner.geometry
    }
    pub fn commands(&self) -> &[FrameCommand] {
        &self.inner.drawing.commands
    }
    pub fn masks(&self) -> &BTreeMap<u32, Arc<FrameMask>> {
        &self.inner.drawing.masks
    }
    pub fn resources(&self) -> &ResourceSnapshot {
        &self.inner.drawing.resources
    }
    pub fn accounted_bytes(&self) -> u64 {
        self.inner.drawing.accounted_bytes
    }
    pub fn reserved_bytes(&self) -> u64 {
        self.inner.drawing._ticket.bytes()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScopeKind {
    Clip,
    Group,
}
#[derive(Debug)]
struct Scope {
    kind: ScopeKind,
    command: usize,
    ink: InkBounds,
}
#[derive(Debug)]
struct Cursor {
    layer: usize,
    path: Vec<record::MaskId>,
    stream: Option<usize>,
    position: usize,
    scopes: Vec<Scope>,
    contexts: Vec<usize>,
    clips: Vec<usize>,
    groups: Vec<usize>,
    owner: WidgetId,
    owner_range: usize,
}
impl Cursor {
    fn storage_bytes(&self) -> usize {
        self.path.capacity() * size_of::<record::MaskId>()
            + self.scopes.capacity() * size_of::<Scope>()
            + (self.contexts.capacity() + self.clips.capacity() + self.groups.capacity())
                * size_of::<usize>()
    }
    fn advance_command(&mut self, plan: &crate::app::VisualLayerPlan) {
        self.position += 1;
        if self.stream.is_some() {
            return;
        }
        let layer = &plan.layers[self.layer];
        // Ranges are contiguous producer metadata. At most one neighbor is read
        // per prepaid commit; never scan after work exhaustion to diagnose it.
        if layer.command_owners[self.owner_range].commands.end == self.position {
            self.owner_range += 1;
            self.owner = layer
                .command_owners
                .get(self.owner_range)
                .map_or(layer.widget_id, |r| r.owner);
        }
    }
    fn advance_fragment(&mut self, plan: &crate::app::VisualLayerPlan) {
        self.layer += 1;
        self.position = 0;
        self.owner_range = 0;
        if let Some(layer) = plan.layers.get(self.layer) {
            self.owner = layer
                .command_owners
                .first()
                .map_or(layer.widget_id, |r| r.owner);
        }
    }
}
#[derive(Debug)]
struct MaskMapping {
    scene: usize,
    local: record::MaskId,
    index: usize,
}
#[derive(Debug)]
struct CopyJob {
    command: FrameCommand,
    field: u8,
    index: usize,
    ink: InkBounds,
    descend: Option<(record::MaskId, usize)>,
}
struct Building {
    source: RecordedSource,
    transitions: Vec<FrameTransition>,
    resources: ResourceSnapshot,
    info: FrameInfo,
    commands: Vec<FrameCommand>,
    masks: Vec<FrameMask>,
    cursors: Vec<Cursor>,
    mappings: Vec<MaskMapping>,
    job: Option<CopyJob>,
    limits: BuildLimits,
    bytes: u64,
    complete: bool,
    start_credit: u64,
    allocation_need: Option<u64>,
    allocation_credit: u64,
    ticket: BudgetTicket,
}
pub struct FrameBuild {
    state: Option<Building>,
    error: Option<BuildError>,
}
impl Drop for FrameBuild {
    fn drop(&mut self) {
        if let Some(state) = &self.state {
            state.source.key.cancel();
        }
    }
}
impl FrameBuild {
    /// Reserves the maximum frame/scratch allowance before any compiler allocation.
    /// Source Scene/cache residency is the producer's separate, measured account.
    pub fn new(
        source: RecordedSource,
        resources: ResourceSnapshot,
        info: FrameInfo,
        limits: BuildLimits,
        pool: &BudgetPool,
    ) -> Result<Self, BuildError> {
        let at = RecordLocation {
            fragment: 0,
            command: 0,
            mask_depth: 0,
            owner: source.plan.layers.first().map_or_else(
                || source.geometry.records()[0].id,
                |l| l.command_owners.first().map_or(l.widget_id, |r| r.owner),
            ),
        };
        let error = |kind| {
            source.key.cancel();
            BuildError { at, kind }
        };
        if source.plan.layers.len() > u32::MAX as usize {
            return Err(error(BuildErrorKind::Capacity));
        }
        if info.logical_size.iter().any(|v| !v.is_finite() || *v < 0.)
            || !source.geometry.scale_factor().is_finite()
            || source.geometry.scale_factor() <= 0.
        {
            return Err(error(BuildErrorKind::InvalidRecord));
        }
        let main_count = source
            .plan
            .layers
            .iter()
            .try_fold(0usize, |n, l| {
                n.checked_add(match &l.kind {
                    VisualLayerKind::Scene(s) => s.commands().len(),
                    VisualLayerKind::External { .. } => 1,
                })
            })
            .ok_or_else(|| error(BuildErrorKind::Capacity))?;
        let ticket = pool
            .reserve(limits.max_bytes)
            .map_err(|_| error(BuildErrorKind::Bytes))?;
        let initial = size_of::<FrameData>()
            .checked_add(4 * size_of::<usize>() + size_of::<Building>() + size_of::<FrameDrawing>())
            .and_then(|n| n.checked_add(main_count.checked_mul(size_of::<FrameCommand>())?))
            .and_then(|n| {
                n.checked_add(
                    source
                        .plan
                        .subtrees
                        .capacity()
                        .checked_mul(size_of::<FrameSubtree>())?,
                )
            })
            .and_then(|n| {
                n.checked_add(
                    source
                        .plan
                        .capture_exclusions
                        .capacity()
                        .checked_mul(size_of::<Range<usize>>())?,
                )
            })
            .and_then(|n| n.checked_add(size_of::<Cursor>()))
            .ok_or_else(|| error(BuildErrorKind::Bytes))?;
        if initial as u64 > limits.max_bytes {
            return Err(error(BuildErrorKind::Bytes));
        }
        let cursor = Cursor {
            layer: 0,
            path: Vec::new(),
            stream: None,
            position: 0,
            scopes: Vec::new(),
            contexts: Vec::new(),
            clips: Vec::new(),
            groups: Vec::new(),
            owner: at.owner,
            owner_range: 0,
        };
        let cursors = vec![cursor];
        Ok(Self {
            state: Some(Building {
                source,
                transitions: Vec::new(),
                resources,
                info,
                commands: Vec::with_capacity(main_count),
                masks: Vec::new(),
                cursors,
                mappings: Vec::new(),
                job: None,
                limits,
                bytes: initial as u64,
                complete: false,
                start_credit: 0,
                allocation_need: None,
                allocation_credit: 0,
                ticket,
            }),
            error: None,
        })
    }
    pub fn set_transitions(&mut self, transitions: Vec<FrameTransition>) -> Result<(), BuildError> {
        let state = self.state.as_mut().expect("unfinished build");
        if !state.commands.is_empty() || state.job.is_some() || state.complete {
            return Err(state.error(BuildErrorKind::InvalidRecord));
        }
        let bytes = transitions
            .iter()
            .try_fold(
                transitions.capacity() * size_of::<FrameTransition>(),
                |bytes, t| {
                    bytes.checked_add(
                        2 * size_of::<SceneTransition>()
                            + 3 * size_of::<Option<WidgetId>>()
                            + size_of::<usize>()
                            + t.mask
                                .as_ref()
                                .map_or(0, |m| m.images.len() * size_of::<ResourceHandle>()),
                    )
                },
            )
            .ok_or_else(|| state.error(BuildErrorKind::Bytes))?;
        state.charge(bytes)?;
        state.transitions = transitions;
        Ok(())
    }
    pub fn step(&mut self, budget: StepBudget) -> Result<BuildProgress, BuildError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let state = self.state.as_mut().expect("unfinished build");
        match state.step(budget) {
            Ok(progress) => Ok(progress),
            Err(error) => {
                state.source.key.cancel();
                self.error = Some(error);
                self.state = None;
                Err(error)
            }
        }
    }
    pub fn cancel(self) {}
    pub fn finish(mut self) -> Result<RenderFrame, BuildError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let state = self.state.take().expect("unfinished build");
        if !state.complete {
            state.source.key.cancel();
            return Err(state.error(BuildErrorKind::Incomplete));
        }
        let Building {
            mut source,
            transitions,
            resources,
            info,
            commands,
            masks,
            bytes,
            ticket,
            ..
        } = state;
        let subtrees = std::mem::take(&mut source.plan.subtrees);
        let body_end = source.plan.body_commands;
        let capture_exclusions = std::mem::take(&mut source.plan.capture_exclusions);
        let (compiled, transition_roots) = retained::compile_transitions(
            &transitions,
            &subtrees,
            body_end,
            &source.geometry,
            &resources,
        )?;
        let RecordedSource {
            key,
            geometry,
            guard,
            ..
        } = source;
        Ok(RenderFrame {
            inner: Arc::new(FrameData {
                key,
                info,
                geometry,
                drawing: Arc::new(FrameDrawing {
                    commands,
                    capture_exclusions,
                    masks: masks
                        .into_iter()
                        .enumerate()
                        .map(|(i, mask)| (i as u32, Arc::new(mask)))
                        .collect(),
                    resources,
                    transitions: compiled,
                    accounted_bytes: bytes,
                    _ticket: ticket,
                }),
                subtrees,
                body_end,
                transition_roots,
                _guard: guard,
            }),
        })
    }
}
impl Building {
    fn error(&self, kind: BuildErrorKind) -> BuildError {
        let c = &self.cursors[0];
        let c = self.cursors.last().unwrap_or(c);
        BuildError {
            kind,
            at: RecordLocation {
                fragment: c.layer,
                command: c.position,
                mask_depth: c.path.len(),
                owner: c.owner,
            },
        }
    }
    fn charge(&mut self, bytes: usize) -> Result<(), BuildError> {
        self.bytes = self
            .bytes
            .checked_add(bytes as u64)
            .filter(|n| *n <= self.limits.max_bytes)
            .ok_or_else(|| self.error(BuildErrorKind::Bytes))?;
        Ok(())
    }
    fn spend(
        &mut self,
        p: &mut BuildProgress,
        b: &StepBudget,
        units: u64,
        bytes: u64,
    ) -> Result<bool, BuildError> {
        if units > b.max_units - p.units || bytes > b.max_bytes - p.bytes {
            return Ok(false);
        }
        p.units += units;
        p.bytes += bytes;
        Ok(true)
    }
    fn scene<'a>(source: &'a RecordedSource, c: &Cursor) -> Option<&'a Scene> {
        let VisualLayerKind::Scene(scene) = &source.plan.layers.get(c.layer)?.kind else {
            return None;
        };
        let mut scene = scene;
        for &mask in &c.path {
            scene = &scene.mask(mask).scene;
        }
        Some(scene)
    }
    fn program(&mut self, stream: Option<usize>) -> &mut Vec<FrameCommand> {
        match stream {
            None => &mut self.commands,
            Some(index) => &mut self.masks[index].commands,
        }
    }
    fn step(&mut self, b: StepBudget) -> Result<BuildProgress, BuildError> {
        let mut p = BuildProgress::default();
        if self.source.key.is_cancelled() {
            return Err(self.error(BuildErrorKind::Cancelled));
        }
        while !self.complete {
            if b.max_commands == 0 {
                break;
            }
            if self.job.is_some() {
                // Payload elements advance in place; move the full command only at commit.
                if self.job.as_ref().unwrap().field < 9 {
                    let resolve_units =
                        2 * self.cursors.last().expect("active cursor").path.len() as u64 + 1;
                    if !self.spend(&mut p, &b, resolve_units, 0)? {
                        p.minimum_units = resolve_units;
                        break;
                    }
                    let c = self.cursors.last().expect("active cursor");
                    let scene = Self::scene(&self.source, c).expect("copy job scene");
                    let cmd = &scene.commands()[c.position];
                    let job = self.job.as_ref().unwrap();
                    let size = field_size(scene, cmd, job.field, job.index);
                    if let Some(size) = size {
                        if !self.spend(&mut p, &b, 0, size as u64)? {
                            p.minimum_units = resolve_units;
                            p.minimum_bytes = size as u64;
                            break;
                        }
                        let scene =
                            Self::scene(&self.source, self.cursors.last().unwrap()).unwrap();
                        let job = self.job.as_mut().unwrap();
                        copy_field(
                            scene,
                            cmd_id(scene, self.cursors.last().unwrap().position),
                            job,
                        )?;
                        job.index += 1;
                    } else {
                        let job = self.job.as_mut().unwrap();
                        job.field += 1;
                        job.index = 0;
                    }
                    continue;
                }
                let cursor = self.cursors.last().unwrap();
                let commit_units = cursor.scopes.len() as u64
                    + cursor.path.len() as u64
                    + if cursor.stream.is_none() { 3 } else { 1 };
                if !self.spend(&mut p, &b, commit_units, 0)? {
                    p.minimum_units = commit_units;
                    break;
                }
                let job = self.job.take().unwrap();
                self.commit(job)?;
                p.commands += 1;
                if p.commands >= b.max_commands {
                    break;
                }
                continue;
            }
            if p.commands >= b.max_commands || p.units >= b.max_units {
                break;
            }
            let c = self.cursors.last().unwrap();
            if c.layer >= self.source.plan.layers.len() {
                if !self.spend(&mut p, &b, 1, 0)? {
                    break;
                }
                let c = self.cursors.last().unwrap();
                if !c.scopes.is_empty() || !c.contexts.is_empty() {
                    return Err(self.error(BuildErrorKind::InvalidRecord));
                }
                self.complete = true;
                break;
            }
            // Prepay even the source-path probe, owner validation/group search and External
            // resource/ink lookup. Credit survives a slice too small for the lookup.
            let group_search = u64::from(
                usize::BITS
                    - self.source.plan.layers[c.layer]
                        .groups
                        .len()
                        .leading_zeros(),
            );
            let owner_work = if c.stream.is_some() {
                0
            } else if matches!(
                self.source.plan.layers[c.layer].kind,
                VisualLayerKind::External { .. }
            ) {
                3
            } else {
                1
            };
            let mapping_work = 2 * self.mappings.len() as u64
                + 4 * c.path.len() as u64
                + 2 * self.resources.leases().len() as u64
                + c.scopes.len() as u64
                + group_search
                + owner_work
                + 8;
            let available = (mapping_work - self.start_credit).min(b.max_units - p.units);
            self.spend(&mut p, &b, available, 0)?;
            self.start_credit += available;
            if self.start_credit < mapping_work {
                p.minimum_units = 1;
                break;
            }
            let c = self.cursors.last().unwrap();
            if let Some(scene) = Self::scene(&self.source, c) {
                if c.position == scene.commands().len() {
                    let advance_units = if c.stream.is_none() { 3 } else { 1 };
                    if !self.spend(&mut p, &b, advance_units, 0)? {
                        p.minimum_units = advance_units;
                        break;
                    }
                    self.start_credit = 0;
                    let c = self.cursors.last().unwrap();
                    if c.stream.is_some() {
                        if !c.scopes.is_empty() || !c.contexts.is_empty() {
                            return Err(self.error(BuildErrorKind::InvalidRecord));
                        }
                        let cursor = self.cursors.pop().unwrap();
                        self.bytes -= cursor.storage_bytes() as u64;
                    } else {
                        self.cursors
                            .last_mut()
                            .unwrap()
                            .advance_fragment(&self.source.plan);
                    }
                    continue;
                }
            } else {
                if !self.spend(&mut p, &b, 0, size_of::<FrameCommand>() as u64)? {
                    p.minimum_bytes = size_of::<FrameCommand>() as u64;
                    break;
                }
                self.start_credit = 0;
                let c = self.cursors.last().unwrap();
                let layer = &self.source.plan.layers[c.layer];
                let VisualLayerKind::External {
                    token,
                    bounds,
                    content_transform,
                } = layer.kind
                else {
                    unreachable!()
                };
                let resource = self
                    .resources
                    .external(token)
                    .ok_or_else(|| self.error(BuildErrorKind::MissingExternal { token }))?;
                let transform = layer.transform * content_transform;
                if !finite(transform) || !bounds.is_finite() {
                    return Err(self.error(BuildErrorKind::InvalidRecord));
                }
                let command = FrameCommand {
                    origin: CommandOrigin {
                        source: c.layer as u32,
                        local: LocalId::None,
                        at: self.error(BuildErrorKind::Incomplete).at,
                    },
                    op: FrameOp::External(ExternalDraw {
                        resource,
                        token,
                        owner: layer.widget_id,
                        local_bounds: bounds,
                        content_to_window: transform,
                    }),
                };
                self.add_ink(inflate_aa(
                    InkBounds::Rect(bounds).transformed(transform),
                    1. / self.source.geometry.scale_factor(),
                ));
                self.commands.push(command);
                p.commands += 1;
                self.cursors
                    .last_mut()
                    .unwrap()
                    .advance_fragment(&self.source.plan);
                continue;
            }
            if self.allocation_need.is_none() {
                if !self.spend(&mut p, &b, 0, size_of::<FrameCommand>() as u64)? {
                    p.minimum_bytes = size_of::<FrameCommand>() as u64;
                    break;
                }
                // Validate the already selected range once after prepayment.
                // Owner was advanced by the prior commit, before any new spend.
                let c = self.cursors.last().unwrap();
                if c.stream.is_none() {
                    let layer = &self.source.plan.layers[c.layer];
                    layer
                        .command_owners
                        .get(c.owner_range)
                        .filter(|range| {
                            range.commands.contains(&c.position) && range.owner == c.owner
                        })
                        .ok_or_else(|| self.error(BuildErrorKind::InvalidRecord))?;
                }
                self.allocation_need = Some(self.next_allocation()?);
            }
            let need = self.allocation_need.expect("prepared allocation");
            let bytes = (need - self.allocation_credit).min(b.max_bytes - p.bytes);
            self.spend(&mut p, &b, 0, bytes)?;
            self.allocation_credit += bytes;
            if self.allocation_credit < need {
                p.minimum_bytes = 1;
                break;
            }
            // Reservation credits may span slices; actual vector reservation is
            // atomic and measured separately from payload copies and source Drop.
            self.start_credit = 0;
            self.allocation_need = None;
            self.allocation_credit = 0;
            self.start(need)?;
        }
        p.complete = self.complete;
        Ok(p)
    }
    fn next_allocation(&self) -> Result<u64, BuildError> {
        let cursor = self.cursors.last().unwrap();
        let scene = Self::scene(&self.source, cursor).unwrap();
        let command = &scene.commands()[cursor.position];
        let mut bytes =
            payload_bytes(scene, command).ok_or_else(|| self.error(BuildErrorKind::Bytes))?;
        let stack_bytes = match command {
            Command::PushClip(_) => growth_bytes(&cursor.scopes)
                .and_then(|n| n.checked_add(growth_bytes(&cursor.clips)?)),
            Command::PushGroup(_) => growth_bytes(&cursor.scopes)
                .and_then(|n| n.checked_add(growth_bytes(&cursor.groups)?)),
            Command::PushContext(_) => growth_bytes(&cursor.contexts),
            _ => Some(0),
        };
        bytes = stack_bytes
            .and_then(|n| n.checked_add(bytes))
            .ok_or_else(|| self.error(BuildErrorKind::Bytes))?;
        if let Command::PushGroup(id) = command
            && let Some(mask) = &scene.group(*id).mask
        {
            let scene_key = scene as *const Scene as usize;
            if !self
                .mappings
                .iter()
                .any(|m| m.scene == scene_key && m.local == mask.mask)
            {
                bytes = scene
                    .mask(mask.mask)
                    .scene
                    .commands()
                    .len()
                    .checked_mul(size_of::<FrameCommand>())
                    .and_then(|n| {
                        n.checked_add(
                            cursor
                                .path
                                .len()
                                .checked_add(1)?
                                .checked_mul(size_of::<record::MaskId>())?,
                        )
                    })
                    .and_then(|n| n.checked_add(growth_bytes(&self.masks)?))
                    .and_then(|n| n.checked_add(growth_bytes(&self.mappings)?))
                    .and_then(|n| n.checked_add(growth_bytes(&self.cursors)?))
                    // Immutable mask Arc and conservative B-tree node storage at publication.
                    .and_then(|n| {
                        n.checked_add(size_of::<FrameMask>() + 2 * size_of::<usize>() + 1024)
                    })
                    .and_then(|n| n.checked_add(bytes))
                    .ok_or_else(|| self.error(BuildErrorKind::Bytes))?;
            }
        }
        if matches!(command, Command::PushGroup(_)) && cursor.stream.is_none() {
            let layer = &self.source.plan.layers[cursor.layer];
            if let Ok(index) = layer
                .groups
                .binary_search_by_key(&cursor.position, |group| group.command)
                && let Some(mask) = &layer.groups[index].image_mask
            {
                bytes = bytes
                    .checked_add(
                        mask.images
                            .len()
                            .checked_mul(size_of::<ResourceHandle>())
                            .ok_or_else(|| self.error(BuildErrorKind::Bytes))?,
                    )
                    .ok_or_else(|| self.error(BuildErrorKind::Bytes))?;
            }
        }
        if (bytes as u64)
            .checked_add(self.bytes)
            .is_none_or(|n| n > self.limits.max_bytes)
        {
            return Err(self.error(BuildErrorKind::Bytes));
        }
        Ok(bytes as u64)
    }
    fn start(&mut self, allocation: u64) -> Result<(), BuildError> {
        self.charge(allocation as usize)?;
        let allocation_error = self.error(BuildErrorKind::Bytes);
        let cursor = self.cursors.last().unwrap();
        let command = &Self::scene(&self.source, cursor).unwrap().commands()[cursor.position];
        let cursor = self.cursors.last_mut().unwrap();
        match command {
            Command::PushClip(_) => {
                reserve_slot(&mut cursor.scopes).map_err(|()| allocation_error)?;
                reserve_slot(&mut cursor.clips).map_err(|()| allocation_error)?;
            }
            Command::PushGroup(_) => {
                reserve_slot(&mut cursor.scopes).map_err(|()| allocation_error)?;
                reserve_slot(&mut cursor.groups).map_err(|()| allocation_error)?;
            }
            Command::PushContext(_) => {
                reserve_slot(&mut cursor.contexts).map_err(|()| allocation_error)?;
            }
            _ => {}
        }
        let c = self.cursors.last().unwrap();
        let layer = &self.source.plan.layers[c.layer];
        let scene = Self::scene(&self.source, c).unwrap();
        let command = &scene.commands()[c.position];
        let at = self.error(BuildErrorKind::Incomplete).at;
        let transform = if c.stream.is_some() {
            Affine::IDENTITY
        } else {
            layer.transform
        };
        if !finite(transform) {
            return Err(self.error(BuildErrorKind::InvalidRecord));
        }
        let source = if let Some(mask) = c.stream {
            self.source
                .plan
                .layers
                .len()
                .checked_add(mask)
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| self.error(BuildErrorKind::Capacity))?
        } else {
            c.layer as u32
        };
        let mut descend = None;
        let mut mask = None;
        if let Command::PushGroup(id) = command
            && let Some(applied) = &scene.group(*id).mask
        {
            let scene_key = scene as *const Scene as usize;
            let found = self
                .mappings
                .iter()
                .find(|m| m.scene == scene_key && m.local == applied.mask)
                .map(|m| m.index);
            let index = found.unwrap_or(self.masks.len());
            let mask_id = u32::try_from(index).map_err(|_| self.error(BuildErrorKind::Capacity))?;
            if found.is_none() {
                let definition = scene.mask(applied.mask);
                let count = definition.scene.commands().len();
                let mode = definition.mode;
                let local = applied.mask;
                let t = transform * applied.transform;
                reserve_slot(&mut self.masks).map_err(|()| allocation_error)?;
                reserve_slot(&mut self.mappings).map_err(|()| allocation_error)?;
                reserve_slot(&mut self.cursors).map_err(|()| allocation_error)?;
                self.masks.push(FrameMask {
                    mode,
                    commands: Vec::with_capacity(count),
                });
                self.mappings.push(MaskMapping {
                    scene: scene_key,
                    local,
                    index,
                });
                descend = Some((local, index));
                mask = Some(MaskUse {
                    mask: mask_id,
                    transform: t,
                });
            } else {
                mask = Some(MaskUse {
                    mask: mask_id,
                    transform: transform * applied.transform,
                });
            }
        }
        let c = self.cursors.last().unwrap();
        let layer = &self.source.plan.layers[c.layer];
        let meta = if c.stream.is_some() {
            Some(GroupMetadata {
                command: c.position,
                owner: c.owner,
                effect_transform: Affine::IDENTITY,
                image_mask: None,
                invert: 0.,
                blend: None,
            })
        } else {
            layer
                .groups
                .binary_search_by_key(&c.position, |m| m.command)
                .ok()
                .map(|i| {
                    let mut metadata = layer.groups[i].clone();
                    metadata.effect_transform = layer.transform * metadata.effect_transform;
                    metadata
                })
        };
        let scene = Self::scene(&self.source, self.cursors.last().unwrap()).unwrap();
        let command = &scene.commands()[self.cursors.last().unwrap().position];
        let image_color = if c.stream.is_none() {
            layer.command_owners[c.owner_range].image_color
        } else {
            [1.; 4]
        };
        let image_appearance = if c.stream.is_none() {
            layer.command_owners[c.owner_range]
                .image_appearance
                .map(|appearance| appearance.transformed(transform))
        } else {
            None
        };
        let (local, op) = make_op(
            scene,
            command,
            transform,
            meta,
            mask,
            ImagePaint {
                color: image_color,
                appearance: image_appearance,
                source_rect: if c.stream.is_none() {
                    layer.command_owners[c.owner_range].image_source_rect
                } else {
                    None
                },
                sampling_transform: c.stream.is_none()
                    && layer.command_owners[c.owner_range].image_sampling_transform,
                resource: if c.stream.is_none() {
                    layer.command_owners[c.owner_range].image_resource
                } else {
                    None
                },
            },
            &self.resources,
        )
        .map_err(|kind| self.error(kind))?;
        validate_op(&op).map_err(|kind| self.error(kind))?;
        let ink = initial_ink(&op);
        self.job = Some(CopyJob {
            command: FrameCommand {
                origin: CommandOrigin { source, local, at },
                op,
            },
            field: 0,
            index: 0,
            ink,
            descend,
        });
        Ok(())
    }
    fn add_ink(&mut self, ink: InkBounds) {
        if let Some(group) = self
            .cursors
            .last_mut()
            .unwrap()
            .scopes
            .iter_mut()
            .rev()
            .find(|s| s.kind == ScopeKind::Group)
        {
            group.ink = group.ink.union(ink);
        }
    }
    fn commit(&mut self, mut job: CopyJob) -> Result<(), BuildError> {
        let c = self.cursors.last().unwrap();
        let stream = c.stream;
        let index = match stream {
            None => self.commands.len(),
            Some(i) => self.masks[i].commands.len(),
        };
        let kind = match &job.command.op {
            FrameOp::PushClip(_) => Some(ScopeKind::Clip),
            FrameOp::PushGroup(_) => Some(ScopeKind::Group),
            _ => None,
        };
        if let Some(kind) = kind {
            let c = self.cursors.last_mut().unwrap();
            match kind {
                ScopeKind::Clip => c.clips.push(index),
                ScopeKind::Group => c.groups.push(index),
            }
            c.scopes.push(Scope {
                kind,
                command: index,
                ink: InkBounds::Empty,
            });
        } else if matches!(job.command.op, FrameOp::PopClip | FrameOp::PopGroup) {
            let expected = if matches!(job.command.op, FrameOp::PopClip) {
                ScopeKind::Clip
            } else {
                ScopeKind::Group
            };
            if c.scopes.last().is_none_or(|s| s.kind != expected) {
                let crossed = match expected {
                    ScopeKind::Clip => !c.clips.is_empty(),
                    ScopeKind::Group => !c.groups.is_empty(),
                };
                return Err(self.error(if crossed {
                    BuildErrorKind::UnsupportedRecord
                } else {
                    BuildErrorKind::InvalidRecord
                }));
            }
            let c = self.cursors.last_mut().unwrap();
            match expected {
                ScopeKind::Clip => {
                    c.clips.pop();
                }
                ScopeKind::Group => {
                    c.groups.pop();
                }
            }
            let scope = c.scopes.pop().unwrap();
            if scope.kind == ScopeKind::Group {
                let FrameOp::PushGroup(group) = &mut self.program(stream)[scope.command].op else {
                    unreachable!()
                };
                // Filters and shaped glyphs may extend arbitrarily beyond layout boxes.
                group.ink_bounds = if group.filters.is_empty() {
                    scope.ink
                } else {
                    InkBounds::Unbounded
                };
                let ink = group.ink_bounds;
                self.add_ink(ink);
            }
        } else if matches!(job.command.op, FrameOp::PushContext(_)) {
            self.cursors.last_mut().unwrap().contexts.push(index);
        } else if matches!(job.command.op, FrameOp::PopContext) {
            if self.cursors.last_mut().unwrap().contexts.pop().is_none() {
                return Err(self.error(BuildErrorKind::InvalidRecord));
            }
        } else if matches!(job.command.op, FrameOp::Draw(_)) {
            let ink = if stream.is_some() {
                // A reusable local mask can be sampled through different transforms.
                // Its target-space AA footprint is resolved by the backend per use.
                InkBounds::Unbounded
            } else {
                draw_ink(
                    &job.command.op,
                    job.ink,
                    1. / self.source.geometry.scale_factor(),
                )
            };
            self.add_ink(ink);
        }
        let owner = match &job.command.op {
            FrameOp::PushGroup(g) => g.owner,
            _ => job.command.origin.at.owner,
        };
        self.program(stream).push(job.command);
        self.cursors
            .last_mut()
            .unwrap()
            .advance_command(&self.source.plan);
        if let Some((mask, index)) = job.descend.take() {
            let parent = self.cursors.last().unwrap();
            let mut path = Vec::with_capacity(parent.path.len() + 1);
            path.extend_from_slice(&parent.path);
            path.push(mask);
            self.cursors.push(Cursor {
                layer: parent.layer,
                path,
                stream: Some(index),
                position: 0,
                scopes: Vec::new(),
                contexts: Vec::new(),
                clips: Vec::new(),
                groups: Vec::new(),
                owner,
                owner_range: 0,
            });
        }
        Ok(())
    }
}

// Amortized growth is charged before reserving; no scene-wide count ceiling or
// worst-case nesting allocation is needed. Calls occur only before a push.
fn growth_bytes<T>(values: &Vec<T>) -> Option<usize> {
    if values.len() < values.capacity() {
        Some(0)
    } else {
        values.capacity().max(1).checked_mul(size_of::<T>())
    }
}

fn reserve_slot<T>(values: &mut Vec<T>) -> Result<(), ()> {
    if values.len() == values.capacity() {
        values
            .try_reserve_exact(values.capacity().max(1))
            .map_err(|_| ())?;
    }
    Ok(())
}

fn finite(t: Affine) -> bool {
    t.as_coeffs().iter().all(|v| v.is_finite())
}
fn valid_color(c: [f32; 4]) -> bool {
    c.iter().all(|v| v.is_finite()) && (0.0..=1.).contains(&c[3])
}
fn valid_path_element(element: PathEl) -> bool {
    match element {
        PathEl::MoveTo(p) | PathEl::LineTo(p) => p.is_finite(),
        PathEl::QuadTo(a, b) => a.is_finite() && b.is_finite(),
        PathEl::CurveTo(a, b, c) => a.is_finite() && b.is_finite() && c.is_finite(),
        PathEl::ClosePath => true,
    }
}
fn valid_dash(dash: f64) -> bool {
    dash.is_finite() && dash >= 0.
}
fn valid_stop(stop: peniko::ColorStop) -> bool {
    stop.offset.is_finite() && valid_color(stop.color.components)
}
fn valid_glyph(glyph: record::Glyph) -> bool {
    glyph.x.is_finite() && glyph.y.is_finite()
}
fn valid_geometry(g: &Geometry) -> bool {
    match g {
        Geometry::Rect(r) => r.is_finite(),
        Geometry::RoundedRect(r) => r.is_finite(),
        Geometry::Path(_) => true,
    }
}
fn valid_stroke(s: &Stroke) -> bool {
    s.width.is_finite()
        && s.width >= 0.
        && s.miter_limit.is_finite()
        && s.miter_limit >= 0.
        && s.dash_offset.is_finite()
}
fn valid_clip(c: &Clip) -> bool {
    match c {
        Clip::Fill {
            transform, shape, ..
        } => finite(*transform) && valid_geometry(shape),
        Clip::Stroke {
            transform,
            shape,
            stroke,
        } => finite(*transform) && valid_geometry(shape) && valid_stroke(stroke),
    }
}
fn valid_brush(b: &FrameBrush) -> bool {
    match b {
        Brush::Solid(c) => valid_color(c.components),
        Brush::Image(i) => {
            valid_color(i.image.color)
                && i.image.source_rect.is_none_or(|rect| {
                    (rect.width() as f32) > 0.
                        && (rect.height() as f32) > 0.
                        && [
                            rect.x0,
                            rect.y0,
                            rect.x1,
                            rect.y1,
                            rect.width(),
                            rect.height(),
                        ]
                        .iter()
                        .all(|v| (*v as f32).is_finite())
                })
                && i.image.appearance.is_none_or(|appearance| {
                    appearance.adjustment.is_valid()
                        && appearance.gradient.is_none_or(|gradient| {
                            gradient.colors.into_iter().all(valid_color)
                                && finite(gradient.transform)
                        })
                })
                && i.sampler.alpha.is_finite()
                && (0.0..=1.).contains(&i.sampler.alpha)
        }
        Brush::Gradient(g) => match g.kind {
            peniko::GradientKind::Linear(g) => g.start.is_finite() && g.end.is_finite(),
            peniko::GradientKind::Radial(g) => {
                g.start_center.is_finite()
                    && g.end_center.is_finite()
                    && g.start_radius.is_finite()
                    && g.end_radius.is_finite()
                    && g.start_radius >= 0.
                    && g.end_radius >= 0.
            }
            peniko::GradientKind::Sweep(g) => {
                g.center.is_finite() && g.start_angle.is_finite() && g.end_angle.is_finite()
            }
        },
    }
}
fn valid_filter(f: Filter) -> bool {
    match f {
        Filter::Flood { color } => valid_color(color.components),
        Filter::Blur {
            std_deviation_x: x,
            std_deviation_y: y,
        } => x.is_finite() && y.is_finite() && x >= 0. && y >= 0.,
        Filter::Offset { dx, dy } => dx.is_finite() && dy.is_finite(),
        Filter::DropShadow {
            dx,
            dy,
            std_deviation_x: x,
            std_deviation_y: y,
            color,
        } => {
            dx.is_finite()
                && dy.is_finite()
                && x.is_finite()
                && y.is_finite()
                && x >= 0.
                && y >= 0.
                && valid_color(color.components)
        }
    }
}
fn validate_op(op: &FrameOp) -> Result<(), BuildErrorKind> {
    let valid = match op {
        FrameOp::PushClip(c) => valid_clip(c),
        FrameOp::PushGroup(g) => g.clip.as_ref().is_none_or(valid_clip),
        FrameOp::Draw(FrameDraw::Fill { shape, brush, .. }) => {
            valid_geometry(shape) && valid_brush(brush)
        }
        FrameOp::Draw(FrameDraw::Stroke {
            shape,
            stroke,
            brush,
            ..
        }) => valid_geometry(shape) && valid_stroke(stroke) && valid_brush(brush),
        FrameOp::Draw(FrameDraw::GlyphRun(g)) => {
            valid_brush(&g.brush)
                && match &g.style {
                    Style::Stroke(s) => valid_stroke(s),
                    _ => true,
                }
        }
        FrameOp::Draw(FrameDraw::BlurredRoundedRect(d)) => {
            d.rect.is_finite()
                && d.radius.is_finite()
                && d.radius >= 0.
                && d.std_dev.is_finite()
                && d.std_dev >= 0.
                && valid_color(d.color.components)
        }
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err(BuildErrorKind::InvalidRecord)
    }
}
fn inflate_aa(ink: InkBounds, padding: f64) -> InkBounds {
    match ink {
        InkBounds::Rect(r) => {
            let bounds = r.inflate(padding, padding);
            if bounds.is_finite() {
                InkBounds::Rect(bounds)
            } else {
                InkBounds::Unbounded
            }
        }
        other => other,
    }
}
fn valid_composite(c: Composite) -> bool {
    c.alpha.is_finite() && (0.0..=1.).contains(&c.alpha)
}
fn geometry_bytes(g: &Geometry) -> Option<usize> {
    match g {
        Geometry::Path(p) => p.elements().len().checked_mul(size_of::<PathEl>()),
        _ => Some(0),
    }
}
fn stroke_bytes(s: &Stroke) -> Option<usize> {
    s.dash_pattern.len().checked_mul(size_of::<f64>())
}
fn brush_bytes(b: &Brush) -> Option<usize> {
    match b {
        Brush::Gradient(g) => g.stops.len().checked_mul(size_of::<peniko::ColorStop>()),
        _ => Some(0),
    }
}
fn clip_bytes(c: &Clip) -> Option<usize> {
    match c {
        Clip::Fill { shape, .. } => geometry_bytes(shape),
        Clip::Stroke { shape, stroke, .. } => {
            geometry_bytes(shape)?.checked_add(stroke_bytes(stroke)?)
        }
    }
}
fn payload_bytes(scene: &Scene, c: &Command) -> Option<usize> {
    match c {
        Command::PushClip(id) => clip_bytes(scene.clip(*id)),
        Command::PushGroup(id) => {
            let g = scene.group(*id);
            g.filters
                .len()
                .checked_mul(size_of::<Filter>())?
                .checked_add(g.clip.as_ref().map_or(Some(0), clip_bytes)?)
        }
        Command::Draw(id) => match scene.draw_op(*id) {
            Draw::Fill { shape, brush, .. } => {
                geometry_bytes(shape)?.checked_add(brush_bytes(brush)?)
            }
            Draw::Stroke {
                shape,
                stroke,
                brush,
                ..
            } => geometry_bytes(shape)?
                .checked_add(stroke_bytes(stroke)?)?
                .checked_add(brush_bytes(brush)?),
            Draw::GlyphRun(g) => g
                .glyphs
                .len()
                .checked_mul(size_of::<record::Glyph>())?
                .checked_add(
                    g.normalized_coords
                        .len()
                        .checked_mul(size_of::<imaging::NormalizedCoord>())?,
                )?
                .checked_add(brush_bytes(&g.brush)?)?
                .checked_add(match &g.style {
                    Style::Stroke(s) => stroke_bytes(s)?,
                    _ => 0,
                }),
            _ => Some(0),
        },
        Command::PushContext(id) => {
            let c = scene.context(*id).as_ref(scene);
            let n = match c.kind {
                imaging::ContextKindRef::Named(s) => s.len(),
                _ => 0,
            };
            n.checked_add(match c.value {
                imaging::ContextValueRef::Str(s) => s.len(),
                _ => 0,
            })?
            .checked_add(c.source.map_or(0, |s| s.file.len()))
        }
        _ => Some(0),
    }
}
fn empty_geometry(g: &Geometry) -> Geometry {
    match g {
        Geometry::Rect(r) => Geometry::Rect(*r),
        Geometry::RoundedRect(r) => Geometry::RoundedRect(*r),
        Geometry::Path(p) => Geometry::Path(BezPath::with_capacity(p.elements().len())),
    }
}
fn empty_stroke(s: &Stroke) -> Stroke {
    let mut stroke = Stroke {
        width: s.width,
        join: s.join,
        miter_limit: s.miter_limit,
        start_cap: s.start_cap,
        end_cap: s.end_cap,
        dash_pattern: Default::default(),
        dash_offset: s.dash_offset,
    };
    stroke.dash_pattern.reserve_exact(s.dash_pattern.len());
    stroke
}
#[derive(Clone, Copy)]
pub(crate) struct ImagePaint {
    pub(crate) color: [f32; 4],
    pub(crate) appearance: Option<ImageAppearance>,
    pub(crate) resource: Option<crate::resources::ResourceImageBrush>,
    pub(crate) sampling_transform: bool,
    pub(crate) source_rect: Option<Rect>,
}
impl Default for ImagePaint {
    fn default() -> Self {
        Self {
            color: [1.; 4],
            appearance: None,
            resource: None,
            sampling_transform: false,
            source_rect: None,
        }
    }
}
/// Frame images store local-to-source maps, including constant source axes.
/// Other brush kinds retain the Painter transform convention.
fn image_sampling_transform(
    brush: &Brush,
    transform: Option<Affine>,
    paint: ImagePaint,
) -> Result<Option<Affine>, BuildErrorKind> {
    if (matches!(brush, Brush::Image(_)) || paint.resource.is_some()) && !paint.sampling_transform {
        transform
            .map(|transform| {
                if transform.determinant() == 0. {
                    Err(BuildErrorKind::InvalidRecord)
                } else {
                    Ok(transform.inverse())
                }
            })
            .transpose()
    } else {
        Ok(transform)
    }
}

fn empty_brush(
    b: &Brush,
    resources: &ResourceSnapshot,
    paint: ImagePaint,
) -> Result<FrameBrush, BuildErrorKind> {
    let ImagePaint {
        color: image_color,
        appearance: image_appearance,
        resource: image_resource,
        ..
    } = paint;
    if let Some(binding) = image_resource {
        if !matches!(b, Brush::Solid(_)) {
            return Err(BuildErrorKind::InvalidRecord);
        }
        if !resources.contains_image(binding.resource) {
            return Err(BuildErrorKind::InvalidRecord);
        }
        return Ok(Brush::Image(ImageBrush {
            image: FrameImage {
                resource: binding.resource,
                color: image_color,
                appearance: image_appearance,
                source_rect: paint.source_rect,
            },
            sampler: binding.sampler,
        }));
    }
    Ok(match b {
        Brush::Solid(c) => Brush::Solid(*c),
        Brush::Gradient(g) => {
            let mut gradient = Gradient {
                kind: g.kind,
                extend: g.extend,
                interpolation_cs: g.interpolation_cs,
                hue_direction: g.hue_direction,
                interpolation_alpha_space: g.interpolation_alpha_space,
                stops: Default::default(),
            };
            gradient.stops.reserve_exact(g.stops.len());
            Brush::Gradient(gradient)
        }
        Brush::Image(i) => Brush::Image(ImageBrush {
            image: FrameImage {
                resource: resources
                    .image(&i.image)
                    .ok_or(BuildErrorKind::MissingResource {
                        blob_id: i.image.data.id(),
                    })?,
                color: image_color,
                appearance: image_appearance,
                source_rect: paint.source_rect,
            },
            sampler: i.sampler,
        }),
    })
}
fn empty_clip(c: &Clip, t: Affine) -> Clip {
    match c {
        Clip::Fill {
            transform,
            shape,
            fill_rule,
        } => Clip::Fill {
            transform: t * *transform,
            shape: empty_geometry(shape),
            fill_rule: *fill_rule,
        },
        Clip::Stroke {
            transform,
            shape,
            stroke,
        } => Clip::Stroke {
            transform: t * *transform,
            shape: empty_geometry(shape),
            stroke: empty_stroke(stroke),
        },
    }
}
fn text(n: usize) -> OwnedText {
    OwnedText(Vec::with_capacity(n))
}
fn make_op(
    scene: &Scene,
    c: &Command,
    t: Affine,
    meta: Option<GroupMetadata>,
    mask: Option<MaskUse>,
    paint: ImagePaint,
    resources: &ResourceSnapshot,
) -> Result<(LocalId, FrameOp), BuildErrorKind> {
    Ok(match c {
        Command::PushClip(id) => (
            LocalId::Clip(*id),
            FrameOp::PushClip(empty_clip(scene.clip(*id), t)),
        ),
        Command::PopClip => (LocalId::None, FrameOp::PopClip),
        Command::PopGroup => (LocalId::None, FrameOp::PopGroup),
        Command::PopContext => (LocalId::None, FrameOp::PopContext),
        Command::PushGroup(id) => {
            let g = scene.group(*id);
            let GroupMetadata {
                owner,
                effect_transform: effect,
                image_mask,
                invert,
                blend,
                ..
            } = meta.ok_or(BuildErrorKind::MissingGroupMetadata)?;
            let image_mask = image_mask
                .map(|mask| {
                    if !mask.bounds.is_finite()
                        || mask.parameters.iter().any(|n| !n.is_finite())
                        || mask.images.len() != mask.program.sources().len()
                    {
                        return Err(BuildErrorKind::InvalidRecord);
                    }
                    let mut images = Vec::with_capacity(mask.images.len());
                    for image in mask.images.iter() {
                        if !resources.contains_image(image.handle()) {
                            return Err(BuildErrorKind::InvalidRecord);
                        }
                        images.push(image.handle());
                    }
                    Ok(FrameImageMask {
                        program: mask.program,
                        images,
                        bounds: mask.bounds,
                        parameters: mask.parameters,
                    })
                })
                .transpose()?;
            if !invert.is_finite()
                || !(0. ..=1.).contains(&invert)
                || !finite(effect)
                || !valid_composite(g.composite)
                || mask.is_some_and(|m| !finite(m.transform))
            {
                return Err(BuildErrorKind::InvalidRecord);
            }
            (
                LocalId::Group(*id),
                FrameOp::PushGroup(FrameGroup {
                    image_mask,
                    clip: g.clip.as_ref().map(|c| empty_clip(c, t)),
                    mask,
                    filters: Vec::with_capacity(g.filters.len()),
                    composite: GroupComposite {
                        alpha: g.composite.alpha,
                        blend: blend.unwrap_or_else(|| g.composite.blend.into()),
                    },
                    invert,
                    owner,
                    effect_to_target: effect,
                    ink_bounds: InkBounds::Empty,
                }),
            )
        }
        Command::PushContext(id) => {
            let c = scene.context(*id).as_ref(scene);
            (
                LocalId::Context(*id),
                FrameOp::PushContext(FrameContext {
                    kind: match c.kind {
                        imaging::ContextKindRef::Label => FrameContextKind::Label,
                        imaging::ContextKindRef::Widget => FrameContextKind::Widget,
                        imaging::ContextKindRef::ChildIndex => FrameContextKind::ChildIndex,
                        imaging::ContextKindRef::Slot => FrameContextKind::Slot,
                        imaging::ContextKindRef::Named(s) => FrameContextKind::Named(text(s.len())),
                    },
                    value: match c.value {
                        imaging::ContextValueRef::Str(s) => FrameContextValue::Str(text(s.len())),
                        imaging::ContextValueRef::U64(v) => FrameContextValue::U64(v),
                        imaging::ContextValueRef::I64(v) => FrameContextValue::I64(v),
                        imaging::ContextValueRef::Usize(v) => FrameContextValue::Usize(v),
                        imaging::ContextValueRef::Bool(v) => FrameContextValue::Bool(v),
                    },
                    source: c.source.map(|s| FrameSourceLocation {
                        file: text(s.file.len()),
                        line: s.line,
                        column: s.column,
                    }),
                }),
            )
        }
        Command::Draw(id) => {
            let draw = match scene.draw_op(*id) {
                Draw::Fill {
                    transform,
                    fill_rule,
                    brush,
                    brush_transform,
                    shape,
                    composite,
                } => FrameDraw::Fill {
                    transform: t * *transform,
                    fill_rule: *fill_rule,
                    brush: empty_brush(brush, resources, paint)?,
                    brush_transform: image_sampling_transform(brush, *brush_transform, paint)?,
                    shape: empty_geometry(shape),
                    composite: *composite,
                },
                Draw::Stroke {
                    transform,
                    stroke,
                    brush,
                    brush_transform,
                    shape,
                    composite,
                } => FrameDraw::Stroke {
                    transform: t * *transform,
                    stroke: empty_stroke(stroke),
                    brush: empty_brush(brush, resources, paint)?,
                    brush_transform: image_sampling_transform(brush, *brush_transform, paint)?,
                    shape: empty_geometry(shape),
                    composite: *composite,
                },
                Draw::GlyphRun(g) => FrameDraw::GlyphRun(FrameGlyphRun {
                    font: resources
                        .font(&g.font)
                        .ok_or(BuildErrorKind::MissingResource {
                            blob_id: g.font.data.id(),
                        })?,
                    font_index: g.font.index,
                    transform: t * g.transform,
                    glyph_transform: g.glyph_transform,
                    brush_transform: image_sampling_transform(
                        &g.brush,
                        g.brush_transform,
                        ImagePaint {
                            resource: None,
                            ..paint
                        },
                    )?,
                    font_size: g.font_size,
                    font_embolden: g.font_embolden,
                    hint: g.hint,
                    normalized_coords: Vec::with_capacity(g.normalized_coords.len()),
                    style: match &g.style {
                        Style::Fill(f) => Style::Fill(*f),
                        Style::Stroke(s) => Style::Stroke(empty_stroke(s)),
                    },
                    glyphs: Vec::with_capacity(g.glyphs.len()),
                    brush: empty_brush(
                        &g.brush,
                        resources,
                        ImagePaint {
                            resource: None,
                            ..paint
                        },
                    )?,
                    composite: g.composite,
                }),
                Draw::BlurredRoundedRect(d) => {
                    FrameDraw::BlurredRoundedRect(imaging::BlurredRoundedRect {
                        transform: t * d.transform,
                        ..*d
                    })
                }
            };
            validate_draw(&draw)?;
            (LocalId::Draw(*id), FrameOp::Draw(draw))
        }
    })
}
fn validate_draw(d: &FrameDraw) -> Result<(), BuildErrorKind> {
    let (t, bt, c) = match d {
        FrameDraw::Fill {
            transform,
            brush_transform,
            composite,
            ..
        }
        | FrameDraw::Stroke {
            transform,
            brush_transform,
            composite,
            ..
        } => (*transform, *brush_transform, *composite),
        FrameDraw::GlyphRun(g) => {
            if !g.font_size.is_finite()
                || g.font_size < 0.
                || !g.font_embolden.x.is_finite()
                || !g.font_embolden.y.is_finite()
                || g.glyph_transform.is_some_and(|t| !finite(t))
            {
                return Err(BuildErrorKind::InvalidRecord);
            }
            (g.transform, g.brush_transform, g.composite)
        }
        FrameDraw::BlurredRoundedRect(d) => (d.transform, None, d.composite),
    };
    if finite(t) && bt.is_none_or(finite) && valid_composite(c) {
        Ok(())
    } else {
        Err(BuildErrorKind::InvalidRecord)
    }
}
fn cmd_id(scene: &Scene, pos: usize) -> &Command {
    &scene.commands()[pos]
}
fn source_geometry<'a>(scene: &'a Scene, c: &Command) -> Option<&'a Geometry> {
    match c {
        Command::PushClip(id) => Some(match scene.clip(*id) {
            Clip::Fill { shape, .. } | Clip::Stroke { shape, .. } => shape,
        }),
        Command::PushGroup(id) => scene.group(*id).clip.as_ref().map(|c| match c {
            Clip::Fill { shape, .. } | Clip::Stroke { shape, .. } => shape,
        }),
        Command::Draw(id) => match scene.draw_op(*id) {
            Draw::Fill { shape, .. } | Draw::Stroke { shape, .. } => Some(shape),
            _ => None,
        },
        _ => None,
    }
}
fn target_geometry(op: &mut FrameOp) -> Option<&mut Geometry> {
    match op {
        FrameOp::PushClip(c) => Some(match c {
            Clip::Fill { shape, .. } | Clip::Stroke { shape, .. } => shape,
        }),
        FrameOp::PushGroup(g) => g.clip.as_mut().map(|c| match c {
            Clip::Fill { shape, .. } | Clip::Stroke { shape, .. } => shape,
        }),
        FrameOp::Draw(FrameDraw::Fill { shape, .. } | FrameDraw::Stroke { shape, .. }) => {
            Some(shape)
        }
        _ => None,
    }
}
fn source_stroke<'a>(scene: &'a Scene, c: &Command) -> Option<&'a Stroke> {
    match c {
        Command::PushClip(id) => match scene.clip(*id) {
            Clip::Stroke { stroke, .. } => Some(stroke),
            _ => None,
        },
        Command::PushGroup(id) => match &scene.group(*id).clip {
            Some(Clip::Stroke { stroke, .. }) => Some(stroke),
            _ => None,
        },
        Command::Draw(id) => match scene.draw_op(*id) {
            Draw::Stroke { stroke, .. } => Some(stroke),
            Draw::GlyphRun(g) => match &g.style {
                Style::Stroke(s) => Some(s),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}
fn target_stroke(op: &mut FrameOp) -> Option<&mut Stroke> {
    match op {
        FrameOp::PushClip(Clip::Stroke { stroke, .. }) => Some(stroke),
        FrameOp::PushGroup(g) => match &mut g.clip {
            Some(Clip::Stroke { stroke, .. }) => Some(stroke),
            _ => None,
        },
        FrameOp::Draw(FrameDraw::Stroke { stroke, .. }) => Some(stroke),
        FrameOp::Draw(FrameDraw::GlyphRun(g)) => match &mut g.style {
            Style::Stroke(s) => Some(s),
            _ => None,
        },
        _ => None,
    }
}
fn source_brush<'a>(scene: &'a Scene, c: &Command) -> Option<&'a Brush> {
    match c {
        Command::Draw(id) => match scene.draw_op(*id) {
            Draw::Fill { brush, .. } | Draw::Stroke { brush, .. } => Some(brush),
            Draw::GlyphRun(g) => Some(&g.brush),
            _ => None,
        },
        _ => None,
    }
}
fn target_brush(op: &mut FrameOp) -> Option<&mut FrameBrush> {
    match op {
        FrameOp::Draw(FrameDraw::Fill { brush, .. } | FrameDraw::Stroke { brush, .. }) => {
            Some(brush)
        }
        FrameOp::Draw(FrameDraw::GlyphRun(g)) => Some(&mut g.brush),
        _ => None,
    }
}
fn source_glyph<'a>(scene: &'a Scene, c: &Command) -> Option<&'a record::GlyphRun> {
    if let Command::Draw(id) = c
        && let Draw::GlyphRun(g) = scene.draw_op(*id)
    {
        Some(g)
    } else {
        None
    }
}
fn context_text<'a>(scene: &'a Scene, c: &Command, field: u8) -> Option<&'a str> {
    let Command::PushContext(id) = c else {
        return None;
    };
    let ctx = scene.context(*id).as_ref(scene);
    match field {
        6 => match ctx.kind {
            imaging::ContextKindRef::Named(s) => Some(s),
            _ => None,
        },
        7 => match ctx.value {
            imaging::ContextValueRef::Str(s) => Some(s),
            _ => None,
        },
        8 => ctx.source.map(|s| s.file),
        _ => None,
    }
}
fn field_size(scene: &Scene, c: &Command, field: u8, index: usize) -> Option<usize> {
    match field {
        0 => match source_geometry(scene, c)? {
            Geometry::Path(p) if index < p.elements().len() => Some(size_of::<PathEl>()),
            _ => None,
        },
        1 => (index < source_stroke(scene, c)?.dash_pattern.len()).then_some(size_of::<f64>()),
        2 => match source_brush(scene, c)? {
            Brush::Gradient(g) if index < g.stops.len() => Some(size_of::<peniko::ColorStop>()),
            _ => None,
        },
        3 => (index < source_glyph(scene, c)?.glyphs.len()).then_some(size_of::<record::Glyph>()),
        4 => (index < source_glyph(scene, c)?.normalized_coords.len())
            .then_some(size_of::<imaging::NormalizedCoord>()),
        5 => {
            if let Command::PushGroup(id) = c {
                (index < scene.group(*id).filters.len()).then_some(size_of::<Filter>())
            } else {
                None
            }
        }
        6..=8 => (index < context_text(scene, c, field)?.len()).then_some(1),
        _ => None,
    }
}
fn copy_field(scene: &Scene, c: &Command, job: &mut CopyJob) -> Result<(), BuildError> {
    let i = job.index;
    let op = &mut job.command.op;
    let error = || BuildError {
        at: job.command.origin.at,
        kind: BuildErrorKind::InvalidRecord,
    };
    match job.field {
        0 => {
            let Geometry::Path(source) = source_geometry(scene, c).unwrap() else {
                unreachable!()
            };
            let Geometry::Path(target) = target_geometry(op).unwrap() else {
                unreachable!()
            };
            let el = source.elements()[i];
            if !valid_path_element(el) {
                return Err(error());
            }
            let points: &[_] = match &el {
                PathEl::MoveTo(p) | PathEl::LineTo(p) => std::slice::from_ref(p),
                _ => &[],
            };
            for p in points {
                job.ink = job.ink.union(InkBounds::Rect(Rect::from_points(*p, *p)));
            }
            match el {
                PathEl::QuadTo(a, b) => {
                    job.ink = job.ink.union(InkBounds::Rect(Rect::from_points(a, b)));
                }
                PathEl::CurveTo(a, b, c) => {
                    job.ink = job.ink.union(InkBounds::Rect(
                        Rect::from_points(a, b).union(Rect::from_points(c, c)),
                    ));
                }
                _ => {}
            }
            target.push(el);
        }
        1 => {
            let dash = source_stroke(scene, c).unwrap().dash_pattern[i];
            if !valid_dash(dash) {
                return Err(error());
            }
            target_stroke(op).unwrap().dash_pattern.push(dash);
        }
        2 => {
            let Brush::Gradient(source) = source_brush(scene, c).unwrap() else {
                unreachable!()
            };
            let Brush::Gradient(target) = target_brush(op).unwrap() else {
                unreachable!()
            };
            let stop = source.stops[i];
            if !valid_stop(stop) {
                return Err(error());
            }
            target.stops.push(stop);
        }
        3 => {
            let glyph = source_glyph(scene, c).unwrap().glyphs[i];
            if !valid_glyph(glyph) {
                return Err(error());
            }
            let FrameOp::Draw(FrameDraw::GlyphRun(g)) = op else {
                unreachable!()
            };
            g.glyphs.push(glyph);
        }
        4 => {
            let FrameOp::Draw(FrameDraw::GlyphRun(g)) = op else {
                unreachable!()
            };
            g.normalized_coords
                .push(source_glyph(scene, c).unwrap().normalized_coords[i]);
        }
        5 => {
            let Command::PushGroup(id) = c else {
                unreachable!()
            };
            let FrameOp::PushGroup(g) = op else {
                unreachable!()
            };
            let filter = scene.group(*id).filters[i];
            if !valid_filter(filter) {
                return Err(error());
            }
            g.filters.push(filter);
        }
        field => {
            let byte = context_text(scene, c, field).unwrap().as_bytes()[i];
            let FrameOp::PushContext(ctx) = op else {
                unreachable!()
            };
            let target = match field {
                6 => {
                    let FrameContextKind::Named(t) = &mut ctx.kind else {
                        unreachable!()
                    };
                    t
                }
                7 => {
                    let FrameContextValue::Str(t) = &mut ctx.value else {
                        unreachable!()
                    };
                    t
                }
                8 => &mut ctx.source.as_mut().unwrap().file,
                _ => unreachable!(),
            };
            target.0.push(byte);
        }
    }
    Ok(())
}
fn initial_ink(op: &FrameOp) -> InkBounds {
    let geometry = match op {
        FrameOp::Draw(FrameDraw::Fill { shape, .. } | FrameDraw::Stroke { shape, .. }) => shape,
        _ => return InkBounds::Empty,
    };
    match geometry {
        Geometry::Rect(r) => InkBounds::Rect(*r),
        Geometry::RoundedRect(r) => InkBounds::Rect(r.rect()),
        Geometry::Path(_) => InkBounds::Empty,
    }
}
fn draw_ink(op: &FrameOp, ink: InkBounds, aa: f64) -> InkBounds {
    match op {
        FrameOp::Draw(FrameDraw::Fill { transform, .. }) => {
            inflate_aa(ink.transformed(*transform), aa)
        }
        FrameOp::Draw(FrameDraw::Stroke {
            transform, stroke, ..
        }) => match ink {
            InkBounds::Rect(r) => inflate_aa(
                InkBounds::Rect(r.inflate(
                    stroke.width * stroke.miter_limit.max(1.),
                    stroke.width * stroke.miter_limit.max(1.),
                ))
                .transformed(*transform),
                aa,
            ),
            other => other,
        },
        // No finite Gaussian kernel cutoff has been fixed by this semantic profile.
        FrameOp::Draw(FrameDraw::BlurredRoundedRect(_)) => InkBounds::Unbounded,
        FrameOp::Draw(FrameDraw::GlyphRun(_)) => InkBounds::Unbounded,
        _ => ink,
    }
}
