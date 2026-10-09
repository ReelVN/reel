//! Real Masonry record/geometry candidates and a bounded presentation acceptance gate.
//!
//! This is the U02 bridge, not a StageGraph or rendering backend. A coordinator turns
//! the renderer's result into a notice containing the candidate's unforgeable key.
use masonry_core::app::{
    GeometryError, GeometryLimits, InputGeometrySource, PresentedGeometryView, RenderRoot,
    VisualLayerKind,
};
use masonry_core::core::WindowEvent;
use masonry_core::core::{Handled, PointerEvent, TextEvent, Widget, WidgetId, WidgetMut};
use masonry_core::imaging::record::Scene;
use reel_core::budget::{BudgetCategory, BudgetPool, BudgetTicket, BudgetUsage, QueueLimits};
use reel_core::identity::{FrameId, SurfaceEpoch};
use std::sync::Arc;

/// Bounded candidate geometry and route storage.
#[derive(Clone, Copy, Debug)]
pub struct BridgeLimits {
    pub geometry: GeometryLimits,
    pub max_candidates: u32,
    /// Fixed route arrays and pending identity registry, reserved before allocation.
    pub max_bridge_bytes: u64,
    /// Each external/accepted geometry lease keeps the maximum export reservation
    /// plus host overhead until its final drop. Aggregate capacity is conservative.
    pub max_snapshot_bytes: u64,
}
/// Opaque identity minted for exactly one actual record operation.
#[derive(Clone, Debug)]
pub struct CandidateKey {
    frame: FrameId,
    epoch: SurfaceEpoch,
    // One allocation is both exact identity and the key/view budget lease.
    identity: Arc<BudgetTicket>,
}
impl CandidateKey {
    pub fn frame_id(&self) -> FrameId {
        self.frame
    }
    pub fn surface_epoch(&self) -> SurfaceEpoch {
        self.epoch
    }
    fn matches(&self, other: &Self) -> bool {
        self.frame == other.frame
            && self.epoch == other.epoch
            && Arc::ptr_eq(&self.identity, &other.identity)
    }
}
/// Scene and geometry from the same real rewrite/compose/paint cycle.
#[derive(Debug)]
pub struct RecordedCandidate {
    key: CandidateKey,
    scene: Scene,
    geometry: Arc<PresentedGeometryView>,
}
impl RecordedCandidate {
    pub fn key(&self) -> CandidateKey {
        self.key.clone()
    }
    /// Full ordered replay: cross-fragment group/clip scope is preserved.
    pub fn scene(&self) -> &Scene {
        &self.scene
    }
    pub fn geometry(&self) -> &PresentedGeometryView {
        &self.geometry
    }
}
/// Renderer outcomes translated by a coordinator, without a render/player dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentationOutcome {
    Accepted(FrameId),
    Skipped,
    SurfaceUnavailable,
    Error,
}
#[derive(Clone, Debug)]
pub struct PresentationNotice {
    pub key: CandidateKey,
    pub surface_epoch: SurfaceEpoch,
    pub outcome: PresentationOutcome,
}
/// Whether an exact candidate was resolved, installed, or ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeResult {
    Accepted,
    NotPresented,
    StaleOrForeign,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordError {
    Geometry(GeometryError),
    SnapshotBudget,
    BridgeBudget,
    CandidateCapacity,
    FrameOrder,
    UnsupportedExternal,
    /// Fixed upstream strips the base layer root transform from paint output.
    /// Use an identity layer root and put stage transforms on its children.
    UnsupportedRootTransform,
    InvalidScene,
}
#[derive(Debug)]
struct Pending {
    key: CandidateKey,
    geometry: Arc<PresentedGeometryView>,
}
/// Owns the accepted input policy and exposes live widget edits and input dispatch.
pub struct PresentationBridge {
    root: RenderRoot,
    epoch: SurfaceEpoch,
    last_issued: Option<FrameId>,
    accepted: Option<(CandidateKey, Arc<PresentedGeometryView>)>,
    pending: Vec<Pending>,
    limits: BridgeLimits,
    snapshots: BudgetPool,
    _routes: BudgetTicket,
}
impl PresentationBridge {
    pub fn new(
        mut root: RenderRoot,
        epoch: SurfaceEpoch,
        limits: BridgeLimits,
    ) -> Result<Self, RecordError> {
        let route_bytes = RenderRoot::input_path_storage_bytes_for(limits.geometry.max_path)
            .ok_or(RecordError::BridgeBudget)?;
        let storage_bytes = route_bytes
            .checked_add(
                (limits.max_candidates as usize)
                    .checked_mul(size_of::<Pending>())
                    .ok_or(RecordError::BridgeBudget)?,
            )
            .ok_or(RecordError::BridgeBudget)?;
        let storage = BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: 1,
                max_bytes: limits.max_bridge_bytes,
            },
        );
        let ticket = storage
            .reserve(storage_bytes as u64)
            .map_err(|_| RecordError::BridgeBudget)?;
        root.set_input_geometry_source(InputGeometrySource::Presented(None));
        root.configure_input_path_storage(limits.geometry.max_path);
        let snapshots = BudgetPool::new(
            BudgetCategory::Snapshot,
            QueueLimits {
                max_requests: limits.max_candidates.saturating_add(1),
                max_bytes: limits.max_snapshot_bytes,
            },
        );
        Ok(Self {
            root,
            epoch,
            last_issued: None,
            accepted: None,
            pending: Vec::with_capacity(limits.max_candidates as usize),
            limits,
            snapshots,
            _routes: ticket,
        })
    }
    pub fn root(&self) -> &RenderRoot {
        &self.root
    }
    /// Ordinary widget mutations receive explicit Live contexts.
    pub fn edit_widget<R>(
        &mut self,
        id: WidgetId,
        edit: impl FnOnce(WidgetMut<'_, dyn Widget>) -> R,
    ) -> R {
        self.root.edit_widget(id, edit)
    }
    /// Dispatch one event through a fixed accepted view.
    pub fn handle_pointer_event(&mut self, event: PointerEvent) -> Handled {
        self.root.handle_pointer_event(event)
    }
    /// Text handling is Live; loss of focus cancels captured pointer input using its view.
    pub fn handle_text_event(&mut self, event: TextEvent) -> Handled {
        self.root.handle_text_event(event)
    }
    /// Platform lifecycle/resize commands also work before the first accepted frame.
    pub fn handle_window_event(&mut self, event: WindowEvent) -> Handled {
        self.root.handle_window_event(event)
    }
    /// Charged fixed route/registry storage, separate from snapshot leases.
    pub fn fixed_storage_bytes(&self) -> u64 {
        self._routes.bytes()
    }
    pub fn snapshot_usage(&self) -> BudgetUsage {
        self.snapshots.usage()
    }
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    pub fn accepted_key(&self) -> Option<&CandidateKey> {
        self.accepted.as_ref().map(|(key, _)| key)
    }
    pub fn record(&mut self, frame: FrameId) -> Result<RecordedCandidate, RecordError> {
        if self.last_issued.is_some_and(|id| frame <= id) {
            return Err(RecordError::FrameOrder);
        }
        if self.pending.len() >= self.limits.max_candidates as usize {
            return Err(RecordError::CandidateCapacity);
        }
        // Reserve the permitted geometry maximum before record/export. Scene and widget
        // runtime storage have separate owners. Keep this same ticket until the view's
        // last lease drops, conservatively retaining the released scratch allowance too.
        let overhead =
            (size_of::<BudgetTicket>() + size_of::<CandidateKey>() + 4 * size_of::<usize>()) as u64;
        let reservation = self
            .snapshots
            .reserve(
                (self.limits.geometry.max_bytes as u64)
                    .checked_add(overhead)
                    .ok_or(RecordError::SnapshotBudget)?,
            )
            .map_err(|_| RecordError::SnapshotBudget)?;
        let (plan, geometry) = self
            .root
            .record_presented_geometry(self.limits.geometry)
            .map_err(RecordError::Geometry)?;
        let base = self.root.get_layer_root(0).id();
        if geometry
            .get(base)
            .is_none_or(|g| g.window_transform != masonry_core::kurbo::Affine::IDENTITY)
        {
            return Err(RecordError::UnsupportedRootTransform);
        }
        if plan
            .layers
            .iter()
            .any(|l| matches!(l.kind, VisualLayerKind::External { .. }))
        {
            return Err(RecordError::UnsupportedExternal);
        }
        let mut scene = Scene::new();
        plan.replay_into(&mut scene);
        scene.validate().map_err(|_| RecordError::InvalidScene)?;
        let lease = Arc::new(reservation);
        let geometry = Arc::new(geometry.with_retention_guard(lease.clone()));
        let key = CandidateKey {
            frame,
            epoch: self.epoch,
            identity: lease,
        };
        self.pending.push(Pending {
            key: key.clone(),
            geometry: geometry.clone(),
        });
        self.last_issued = Some(frame);
        Ok(RecordedCandidate {
            key,
            scene,
            geometry,
        })
    }
    pub fn resolve(&mut self, notice: PresentationNotice) -> NoticeResult {
        if notice.key.epoch != self.epoch || notice.surface_epoch != self.epoch {
            return NoticeResult::StaleOrForeign;
        }
        if let PresentationOutcome::Accepted(frame) = notice.outcome
            && frame != notice.key.frame
        {
            return NoticeResult::StaleOrForeign;
        }
        let Some(index) = self.pending.iter().position(|p| p.key.matches(&notice.key)) else {
            return NoticeResult::StaleOrForeign;
        };
        let candidate = self.pending.remove(index);
        if !matches!(notice.outcome, PresentationOutcome::Accepted(_)) {
            return NoticeResult::NotPresented;
        }
        if self
            .accepted
            .as_ref()
            .is_some_and(|(key, _)| key.frame >= candidate.key.frame)
        {
            return NoticeResult::StaleOrForeign;
        }
        self.root
            .set_input_geometry_source(InputGeometrySource::Presented(Some(
                candidate.geometry.clone(),
            )));
        self.accepted = Some((candidate.key, candidate.geometry));
        NoticeResult::Accepted
    }
    /// Clear accepted input and pending identities; outstanding renderer leases stay charged.
    pub fn clear(&mut self) {
        self.root
            .set_input_geometry_source(InputGeometrySource::Presented(None));
        self.accepted = None;
        self.pending.clear();
    }
    /// A new surface must use a strictly newer epoch. Old exits finish before releasing the view.
    pub fn replace_surface(&mut self, epoch: SurfaceEpoch) -> bool {
        if epoch <= self.epoch {
            return false;
        }
        self.clear();
        self.epoch = epoch;
        true
    }
}
