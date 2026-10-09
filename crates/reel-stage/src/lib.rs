//! Real Masonry record/geometry candidates and a bounded presentation acceptance gate.
//!
//! R01 compiles this record directly; U02 input ownership remains unchanged. A coordinator turns
//! the renderer's result into a notice containing the candidate's unforgeable key.
use masonry_core::app::{
    GeometryError, GeometryLimits, InputGeometrySource, PresentedGeometryView, RenderRoot,
    VisualLayerPlan,
};
use masonry_core::core::WindowEvent;
use masonry_core::core::{Handled, PointerEvent, TextEvent, Widget, WidgetId, WidgetMut};
use masonry_core::imaging::record::Scene;
use reel_core::budget::{BudgetCategory, BudgetPool, BudgetTicket, BudgetUsage, QueueLimits};
use reel_core::identity::{FrameId, SurfaceEpoch};
use std::sync::{
    Arc, OnceLock, Weak,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub mod frame;
pub mod resources;
pub use frame::*;
pub use resources::*;

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
    identity: Arc<CandidateIdentity>,
}
impl CandidateKey {
    pub fn frame_id(&self) -> FrameId {
        self.frame
    }
    pub fn surface_epoch(&self) -> SurfaceEpoch {
        self.epoch
    }
}
#[derive(Debug)]
struct CandidateIdentity {
    cancelled: AtomicBool,
    // Keys and geometry retain accounting, but only guards retain eligibility.
    owners: AtomicUsize,
    geometry: OnceLock<Weak<PresentedGeometryView>>,
    _ticket: BudgetTicket,
}
#[derive(Debug)]
pub(crate) struct CandidateGuard {
    key: CandidateKey,
}
impl CandidateGuard {
    fn retain(key: &CandidateKey) -> Option<Self> {
        key.identity
            .owners
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |owners| {
                (!key.identity.cancelled.load(Ordering::Acquire) && owners != 0)
                    .then(|| owners.checked_add(1))
                    .flatten()
            })
            .ok()?;
        Some(Self { key: key.clone() })
    }
}
impl Clone for CandidateGuard {
    fn clone(&self) -> Self {
        // A live guard always has a nonzero count, including after explicit clear.
        self.key
            .identity
            .owners
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |owners| {
                owners.checked_add(1)
            })
            .expect("candidate guard count exhausted");
        Self {
            key: self.key.clone(),
        }
    }
}
impl Drop for CandidateGuard {
    fn drop(&mut self) {
        if self.key.identity.owners.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.key.identity.cancelled.store(true, Ordering::Release);
        }
    }
}
/// One real paint/geometry cycle, moved into the resumable compiler without flattening.
#[derive(Debug)]
pub struct RecordedSource {
    guard: CandidateGuard,
    key: CandidateKey,
    plan: VisualLayerPlan,
    geometry: Arc<PresentedGeometryView>,
    reference_scene: OnceLock<Scene>,
}
/// U02 source-compatible name. scene() is an explicit, lazy structural reference.
pub type RecordedCandidate = RecordedSource;
impl RecordedSource {
    pub fn key(&self) -> CandidateKey {
        self.key.clone()
    }
    pub fn plan(&self) -> &VisualLayerPlan {
        &self.plan
    }
    /// Compatibility/CPU reference only. Production FrameBuild never calls this.
    pub fn scene(&self) -> &Scene {
        self.reference_scene.get_or_init(|| {
            let mut scene = Scene::new();
            self.plan.replay_into(&mut scene);
            scene
        })
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
    key: CandidateKey,
    surface_epoch: SurfaceEpoch,
    outcome: PresentationOutcome,
    _retention: Option<AcceptedRetention>,
}
#[derive(Clone, Debug)]
struct AcceptedRetention {
    _guard: CandidateGuard,
    _geometry: Arc<PresentedGeometryView>,
}
impl PresentationNotice {
    pub fn key(&self) -> &CandidateKey {
        &self.key
    }
    pub fn surface_epoch(&self) -> SurfaceEpoch {
        self.surface_epoch
    }
    pub fn outcome(&self) -> PresentationOutcome {
        self.outcome
    }
    /// An Accepted notice owns eligibility and matching geometry independently
    /// of the frame or GPU submission. An already cancelled key cannot revive it.
    pub fn new(
        key: CandidateKey,
        surface_epoch: SurfaceEpoch,
        outcome: PresentationOutcome,
    ) -> Self {
        let retention = if matches!(outcome, PresentationOutcome::Accepted(_)) {
            // Upgrade geometry first so a concurrent final frame drop cannot
            // destroy it between acquiring eligibility and acquiring the view.
            key.identity
                .geometry
                .get()
                .and_then(Weak::upgrade)
                .and_then(|geometry| {
                    CandidateGuard::retain(&key).map(|guard| AcceptedRetention {
                        _guard: guard,
                        _geometry: geometry,
                    })
                })
        } else {
            None
        };
        Self {
            key,
            surface_epoch,
            outcome,
            _retention: retention,
        }
    }
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
}
#[derive(Debug)]
struct Pending {
    frame: FrameId,
    epoch: SurfaceEpoch,
    identity: Weak<CandidateIdentity>,
    geometry: Weak<PresentedGeometryView>,
}
impl Pending {
    fn eligible(&self) -> bool {
        self.identity
            .upgrade()
            .is_some_and(|id| !id.cancelled.load(Ordering::Acquire))
    }
    fn matches(&self, key: &CandidateKey) -> bool {
        self.frame == key.frame
            && self.epoch == key.epoch
            && self.identity.ptr_eq(&Arc::downgrade(&key.identity))
    }
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
                    .checked_mul(
                        size_of::<Pending>()
                            + size_of::<CandidateIdentity>()
                            + size_of::<PresentedGeometryView>()
                            + 4 * size_of::<usize>(),
                    )
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
        self.pending.iter().filter(|p| p.eligible()).count()
    }
    pub fn accepted_key(&self) -> Option<&CandidateKey> {
        self.accepted.as_ref().map(|(key, _)| key)
    }
    pub fn record(&mut self, frame: FrameId) -> Result<RecordedSource, RecordError> {
        self.pending.retain(Pending::eligible);
        if self.last_issued.is_some_and(|id| frame <= id) {
            return Err(RecordError::FrameOrder);
        }
        if self.pending.len() >= self.limits.max_candidates as usize {
            return Err(RecordError::CandidateCapacity);
        }
        // Reserve the permitted geometry maximum before record/export. Scene and widget
        // runtime storage have separate owners. Keep this same ticket until the view's
        // last lease drops, conservatively retaining the released scratch allowance too.
        let overhead = (size_of::<CandidateIdentity>()
            + size_of::<CandidateKey>()
            + 4 * size_of::<usize>()) as u64;
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
        let lease = Arc::new(CandidateIdentity {
            cancelled: AtomicBool::new(false),
            owners: AtomicUsize::new(1),
            geometry: OnceLock::new(),
            _ticket: reservation,
        });
        let geometry = Arc::new(geometry.with_retention_guard(lease.clone()));
        lease
            .geometry
            .set(Arc::downgrade(&geometry))
            .expect("new candidate geometry");
        let key = CandidateKey {
            frame,
            epoch: self.epoch,
            identity: lease,
        };
        self.pending.push(Pending {
            frame,
            epoch: self.epoch,
            identity: Arc::downgrade(&key.identity),
            geometry: Arc::downgrade(&geometry),
        });
        self.last_issued = Some(frame);
        Ok(RecordedSource {
            guard: CandidateGuard { key: key.clone() },
            key,
            plan,
            geometry,
            reference_scene: OnceLock::new(),
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
        let Some(index) = self.pending.iter().position(|p| p.matches(&notice.key)) else {
            return NoticeResult::StaleOrForeign;
        };
        let candidate = self.pending.remove(index);
        if !candidate.eligible() {
            return NoticeResult::StaleOrForeign;
        }
        if !matches!(notice.outcome, PresentationOutcome::Accepted(_)) {
            notice.key.identity.cancelled.store(true, Ordering::Release);
            return NoticeResult::NotPresented;
        }
        if self
            .accepted
            .as_ref()
            .is_some_and(|(key, _)| key.frame >= candidate.frame)
        {
            return NoticeResult::StaleOrForeign;
        }
        let Some(geometry) = candidate.geometry.upgrade() else {
            return NoticeResult::StaleOrForeign;
        };
        self.root
            .set_input_geometry_source(InputGeometrySource::Presented(Some(geometry.clone())));
        self.accepted = Some((notice.key, geometry));
        NoticeResult::Accepted
    }
    /// Clear accepted input and pending identities; outstanding renderer leases stay charged.
    pub fn clear(&mut self) {
        self.root
            .set_input_geometry_source(InputGeometrySource::Presented(None));
        self.accepted = None;
        for pending in &self.pending {
            if let Some(id) = pending.identity.upgrade() {
                id.cancelled.store(true, Ordering::Release);
            }
        }
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
