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
use std::{
    collections::BTreeMap,
    hash::{Hash, Hasher},
    mem::size_of,
    ops::Range,
    sync::{Arc, Weak},
};

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
/// Stable identity of an immutable recording, including an embedded mask scene.
///
/// Equality uses the root allocation and its complete mask path. The weak root
/// does not retain recording payloads. Consumers must check that it is live and
/// include resolved transforms, resources and drawing state in their cache keys.
/// This identity is process-local and is never written to retained archives.
#[derive(Clone, Debug)]
pub struct RecordingSource {
    root: Weak<Scene>,
    mask_path: Vec<record::MaskId>,
}
impl RecordingSource {
    /// Root recording; upgrading it also keeps every embedded mask scene alive.
    pub fn root(&self) -> &Weak<Scene> {
        &self.root
    }
    /// Mask IDs from the root to this source; empty for the root scene itself.
    pub fn mask_path(&self) -> &[record::MaskId] {
        &self.mask_path
    }
    /// Whether the original immutable recording still has a strong owner.
    pub fn is_live(&self) -> bool {
        self.root.strong_count() != 0
    }
}
impl PartialEq for RecordingSource {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.root, &other.root) && self.mask_path == other.mask_path
    }
}
impl Eq for RecordingSource {}
impl Hash for RecordingSource {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.root.as_ptr().hash(state);
        self.mask_path.hash(state);
    }
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
/// A frozen shader use; the resource descriptor supplies its immutable parameter
/// schema and output type. Values belong to this frame, while code remains leased.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FrameEffect {
    pub definition: crate::stage::effect::EffectDefinitionId,
    pub shader: ResourceHandle,
    pub values: Vec<crate::stage::effect::EffectValue>,
    /// Whole-image resource bindings retained by this frame's resource snapshot.
    pub images: Vec<ResourceHandle>,
    /// Finite output domain in the node's local logical coordinates.
    pub bounds: Rect,
    /// Invert clamped scalar coverage; preserved by retained scene archives.
    pub coverage_invert: bool,
    /// Linear output resolution multiplier in (0, 1].
    pub resolution_scale: f64,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FrameGroup {
    pub image_mask: Option<FrameImageMask>,
    pub effect: Option<FrameEffect>,
    /// Scalar shader coverage applied after the subtree's color effect.
    pub mask_effect: Option<FrameEffect>,
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
mod sprites;
pub use sprites::{FrameSprite, FrameSprites, SpriteInstance, SpriteInstances};
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
    Sprites(FrameSprites),
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
    recordings: Vec<Option<RecordingSource>>,
    capture_exclusions: Vec<Range<usize>>,
    masks: BTreeMap<u32, Arc<FrameMask>>,
    resources: ResourceSnapshot,
    transitions: Vec<SceneTransition>,
    accounted_bytes: u64,
    _ticket: BudgetTicket,
}
type FrameSubtree = crate::app::SubtreePaint;
impl FrameDrawing {
    /// Resolve a command's live recording identity and its local payload ID.
    ///
    /// Source indices only identify entries within this drawing. Compare the
    /// returned recording identities across frames instead. External commands,
    /// scope endings, expired recordings and restored archives return `None`.
    pub fn recording(&self, origin: CommandOrigin) -> Option<(&RecordingSource, LocalId)> {
        let source = self.recordings.get(origin.source as usize)?.as_ref()?;
        (origin.local != LocalId::None && source.is_live()).then_some((source, origin.local))
    }
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
        if layer
            .command_owners()
            .get(self.owner_range)
            .is_some_and(|r| r.commands.end == self.position)
        {
            self.owner_range += 1;
            self.owner = layer
                .command_owners()
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
                .command_owners()
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
    recordings: Vec<Option<RecordingSource>>,
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
    /// Reserves each frame/scratch allocation before growing its storage.
    /// The per-frame limit bounds this build; the pool bounds simultaneous frames.
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
                |l| l.command_owners().first().map_or(l.widget_id, |r| r.owner),
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
        let initial = size_of::<FrameData>()
            .checked_add(4 * size_of::<usize>() + size_of::<Building>() + size_of::<FrameDrawing>())
            .and_then(|n| n.checked_add(main_count.checked_mul(size_of::<FrameCommand>())?))
            .and_then(|n| {
                n.checked_add(
                    source
                        .plan
                        .layers
                        .len()
                        .checked_mul(size_of::<Option<RecordingSource>>())?,
                )
            })
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
        let ticket = pool
            .reserve(initial as u64)
            .map_err(|_| error(BuildErrorKind::Bytes))?;
        let recordings = source
            .plan
            .layers
            .iter()
            .map(|layer| match &layer.kind {
                VisualLayerKind::Scene(scene) => Some(RecordingSource {
                    root: Arc::downgrade(scene),
                    mask_path: Vec::new(),
                }),
                VisualLayerKind::External { .. } => None,
            })
            .collect();
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
                recordings,
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
            recordings,
            masks,
            bytes,
            mut ticket,
            cursors,
            mappings,
            ..
        } = state;
        let scratch = size_of::<Building>()
            + cursors.capacity() * size_of::<Cursor>()
            + cursors.iter().map(Cursor::storage_bytes).sum::<usize>()
            + mappings.capacity() * size_of::<MaskMapping>()
            + masks.capacity() * size_of::<FrameMask>()
            + transitions.capacity() * size_of::<FrameTransition>();
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
        let masks = masks
            .into_iter()
            .enumerate()
            .map(|(i, mask)| (i as u32, Arc::new(mask)))
            .collect();
        drop((cursors, mappings, transitions));
        let bytes = bytes - scratch as u64;
        ticket.resize(bytes).expect("shrinking a frame reservation");
        Ok(RenderFrame {
            inner: Arc::new(FrameData {
                key,
                info,
                geometry,
                drawing: Arc::new(FrameDrawing {
                    commands,
                    recordings,
                    capture_exclusions,
                    masks,
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
mod build;

impl Building {}

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

mod validation;
use validation::{
    finite, inflate_aa, valid_brush, valid_clip, valid_composite, valid_dash, valid_filter,
    valid_geometry, valid_glyph, valid_path_element, valid_stop, valid_stroke, validate_op,
};
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
#[derive(Clone, Copy, Debug, PartialEq)]
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
impl ImagePaint {
    pub(crate) fn is_default(self) -> bool {
        self.color == [1.; 4]
            && self.appearance.is_none()
            && self.resource.is_none()
            && !self.sampling_transform
            && self.source_rect.is_none()
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
                effect: prepared_effect,
                mask_effect: prepared_mask,
                invert,
                blend,
                ..
            } = meta.ok_or(BuildErrorKind::MissingGroupMetadata)?;
            let prepare_effect = |value: crate::core::PaintEffect, coverage: bool| {
                let valid = resources.leases().iter().any(|lease| lease.handle() == value.shader.handle()
                    && matches!(lease.view(), crate::resources::ResourceView::EffectShader { descriptor, .. }
                        if descriptor.matches_instance(&value.instance)
                            && (!coverage || descriptor.output == crate::stage::effect::EffectType::Float)));
                if !valid
                    || !value.bounds.is_finite()
                    || !(value.resolution_scale > 0. && value.resolution_scale <= 1.)
                    || value.images.len() != value.instance.textures().len()
                    || value.images.iter().any(|image| {
                        !resources.leases().iter().any(|lease| {
                            lease.handle() == image.handle()
                                && (lease.image_descriptor().is_some()
                                    || lease.texture_descriptor().is_some())
                        })
                    })
                {
                    return Err(BuildErrorKind::InvalidRecord);
                }
                Ok(FrameEffect {
                    definition: value.instance.definition(),
                    shader: value.shader.handle(),
                    values: value.instance.values().to_vec(),
                    images: value.images.iter().map(|image| image.handle()).collect(),
                    bounds: value.bounds,
                    coverage_invert: value.coverage_invert,
                    resolution_scale: value.resolution_scale,
                })
            };
            let prepared_effect = prepared_effect
                .map(|value| prepare_effect(value, false))
                .transpose()?;
            let prepared_mask = prepared_mask
                .map(|value| prepare_effect(value, true))
                .transpose()?;
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
                    effect: prepared_effect,
                    mask_effect: prepared_mask,
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
        FrameDraw::Sprites(d) => (d.transform, None, d.composite),
    };
    if finite(t) && bt.is_none_or(finite) && valid_composite(c) {
        Ok(())
    } else {
        Err(BuildErrorKind::InvalidRecord)
    }
}

mod copy;
use copy::{cmd_id, copy_field, draw_ink, field_size, initial_ink, source_glyph};
