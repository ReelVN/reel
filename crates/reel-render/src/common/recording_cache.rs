//! Bounded immutable geometry keyed by a live source recording.
use super::{GpuError, mesh, renderer::Budgets};
use reel_core::budget::BudgetTicket;
use reel_ui::frame::{CommandOrigin, FrameDrawing, LocalId, RecordingSource};
use reel_ui::kurbo::Affine;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

// Optional CPU geometry leaves half the transient budget to scratch/uploads;
// one entry occupies at most one eighth. Pressure evicts idle entries.
const CACHE_SHARE: u64 = 2;
const ENTRY_SHARE: u64 = 8;
fn admit(budgets: &Budgets, limit: u64, bytes: u64) -> Option<BudgetTicket> {
    if bytes > limit / ENTRY_SHARE
        || budgets.transient.usage().bytes.saturating_add(bytes) > limit / CACHE_SHARE
    {
        return None;
    }
    budgets.transient.reserve(bytes).ok()
}
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct ShapeDependency {
    pub transform: Affine,
    pub size: [u32; 2],
    pub uv: Affine,
    pub color: [f32; 4],
    pub sampling: mesh::ImageSampling,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct GlyphDependency {
    pub transform: Affine,
    pub size: [u32; 2],
    pub color: [f32; 4],
    pub font: reel_ui::resources::ResourceHandle,
}

pub(crate) struct Glyphs<P, G> {
    pub segments: Vec<(Arc<Mesh>, P)>,
    pub leases: Vec<(u32, G)>,
    _ticket: BudgetTicket,
}
pub(crate) trait StorageSize {
    fn bytes(&self) -> u64;
}
impl<P, G> StorageSize for Glyphs<P, G> {
    fn bytes(&self) -> u64 {
        self._ticket.bytes()
            + self
                .segments
                .iter()
                .map(|(mesh, _)| mesh.bytes())
                .sum::<u64>()
    }
}
impl<P, G> Glyphs<P, G> {
    pub fn new(budgets: &Budgets, limit: u64) -> Option<Self> {
        let ticket = admit(budgets, limit, 0)?;
        Some(Self {
            segments: Vec::new(),
            leases: Vec::new(),
            _ticket: ticket,
        })
    }
    pub fn lease(capture: &mut Option<Self>, id: u32, glyph: G, budgets: &Budgets, limit: u64) {
        let Some(value) = capture else {
            return;
        };
        let Err(index) = value.leases.binary_search_by_key(&id, |(id, _)| *id) else {
            return;
        };
        if grow(&mut value.leases, &mut value._ticket, budgets, limit).is_none() {
            *capture = None;
            return;
        }
        value.leases.insert(index, (id, glyph));
    }
    pub fn capture(
        capture: &mut Option<Self>,
        mesh: &mut mesh::Mesh,
        page: P,
        budgets: &Budgets,
        limit: u64,
    ) {
        if capture.is_none() {
            return;
        }
        if let Some(cached) = Mesh::copy(mesh, budgets, limit) {
            let value = capture.as_mut().unwrap();
            if grow(&mut value.segments, &mut value._ticket, budgets, limit).is_none() {
                *capture = None;
                return;
            }
            value.segments.push((cached, page));
        } else {
            *capture = None;
        }
    }
}
fn grow<T>(
    values: &mut Vec<T>,
    ticket: &mut BudgetTicket,
    budgets: &Budgets,
    limit: u64,
) -> Option<()> {
    if values.len() < values.capacity() {
        return Some(());
    }
    let previous = ticket.bytes();
    let old = (values.capacity() * size_of::<T>()) as u64;
    let capacity = values.capacity().saturating_mul(2).max(4);
    let additional = (capacity * size_of::<T>()) as u64;
    if previous + additional > limit / ENTRY_SHARE
        || budgets.transient.usage().bytes.saturating_add(additional) > limit / CACHE_SHARE
    {
        return None;
    }
    ticket.resize(previous + additional).ok()?;
    values.reserve_exact(capacity - values.len());
    ticket
        .resize(previous - old + (values.capacity() * size_of::<T>()) as u64)
        .expect("shrinking metadata reservation");
    Some(())
}

/// A payload keeps any atlas allocation leases referenced by its vertices alive.
pub(crate) struct Entry<D, T> {
    source: RecordingSource,
    local: LocalId,
    dependency: D,
    value: Option<T>,
    _ticket: BudgetTicket,
}
impl<D, T> Entry<D, T> {
    pub fn value(&self) -> &T {
        self.value
            .as_ref()
            .expect("cache lookup returns resident payloads")
    }
}
pub(crate) struct Cache<D, T> {
    entries: [Option<Arc<Entry<D, T>>>; 128],
    next: usize,
}
impl<D, T> Default for Cache<D, T> {
    fn default() -> Self {
        Self {
            entries: std::array::from_fn(|_| None),
            next: 0,
        }
    }
}
impl<D: PartialEq + Clone, T: StorageSize> Cache<D, T> {
    /// Repeatedly changing projections retain one small candidate, not a fresh
    /// geometry copy every frame. Only a repeated dependency admits the payload.
    pub fn observe(
        &mut self,
        frame: &FrameDrawing,
        origin: CommandOrigin,
        dependency: &D,
        budgets: &Budgets,
        limit: u64,
    ) -> bool {
        let Some((source, local)) = frame.recording(origin) else {
            return false;
        };
        if self.entries.iter().flatten().any(|entry| {
            entry.source == *source && entry.local == local && entry.dependency == *dependency
        }) {
            return true;
        }
        self.store(frame, origin, dependency.clone(), None, budgets, limit);
        false
    }
    pub fn get(
        &mut self,
        frame: &FrameDrawing,
        origin: CommandOrigin,
        dependency: &D,
    ) -> Option<Arc<Entry<D, T>>> {
        let recording = frame.recording(origin);
        for entry in &mut self.entries {
            if entry.as_ref().is_some_and(|entry| !entry.source.is_live()) {
                *entry = None;
            }
            if let Some((source, local)) = recording
                && let Some(entry) = entry
                && entry.source == *source
                && entry.local == local
                && entry.dependency == *dependency
                && entry.value.is_some()
            {
                return Some(entry.clone());
            }
        }
        None
    }
    /// Optional admission cannot turn an otherwise admissible draw into failure.
    /// Callers reserve variable payload storage separately before allocating it.
    pub fn insert(
        &mut self,
        frame: &FrameDrawing,
        origin: CommandOrigin,
        dependency: D,
        value: T,
        budgets: &Budgets,
        limit: u64,
    ) {
        self.store(frame, origin, dependency, Some(value), budgets, limit);
    }
    fn store(
        &mut self,
        frame: &FrameDrawing,
        origin: CommandOrigin,
        dependency: D,
        value: Option<T>,
        budgets: &Budgets,
        limit: u64,
    ) {
        let Some((source, local)) = frame.recording(origin) else {
            return;
        };
        if let Some(entry) = self
            .entries
            .iter_mut()
            .flatten()
            .find(|entry| entry.source == *source && entry.local == local)
            && let Some(entry) = Arc::get_mut(entry)
        {
            entry.dependency = dependency;
            entry.value = value;
            return;
        }
        let bytes = (size_of::<Entry<D, T>>()
            + 2 * size_of::<usize>()
            + std::mem::size_of_val(source.mask_path())) as u64;
        let Some(ticket) = admit(budgets, limit, bytes) else {
            return;
        };
        let index = self
            .entries
            .iter()
            .position(|entry| {
                entry
                    .as_ref()
                    .is_some_and(|entry| entry.source == *source && entry.local == local)
            })
            .unwrap_or(self.next);
        self.entries[index] = Some(Arc::new(Entry {
            source: source.clone(),
            local,
            dependency,
            value,
            _ticket: ticket,
        }));
        if index == self.next {
            self.next = (self.next + 1) % self.entries.len();
        }
    }
    pub fn clear(&mut self) {
        self.entries.fill_with(|| None);
    }
    pub fn bytes(&self) -> u64 {
        self.entries
            .iter()
            .flatten()
            .map(|entry| {
                entry._ticket.bytes() + entry.value.as_ref().map_or(0, |value| value.bytes())
            })
            .sum()
    }
}

/// A cache copy owns its global reservation; each replay copy gets a fresh
/// preparation reservation, so an idle cache never retains a preparation quota.
pub(crate) struct Mesh {
    pub mesh: mesh::Mesh,
    _ticket: BudgetTicket,
}
impl Mesh {
    pub fn copy(source: &mut mesh::Mesh, budgets: &Budgets, limit: u64) -> Option<Arc<Self>> {
        let bytes = source.bytes() as u64 + (size_of::<Self>() + 2 * size_of::<usize>()) as u64;
        let ticket = admit(budgets, limit, bytes)?;
        static NEXT: AtomicU64 = AtomicU64::new(1);
        source.identity = Some(NEXT.fetch_add(1, Ordering::Relaxed));
        Some(Arc::new(Self {
            mesh: source.cache_copy(),
            _ticket: ticket,
        }))
    }
    pub fn borrow(self: &Arc<Self>, allocation: mesh::Allocation) -> Result<Reference, GpuError> {
        let ticket = allocation.borrow_cached(self.mesh.bytes() as u64)?;
        Ok(Reference::Shared {
            mesh: self.clone(),
            _ticket: ticket,
            allocation,
        })
    }
}
impl StorageSize for Arc<Mesh> {
    fn bytes(&self) -> u64 {
        self._ticket.bytes()
    }
}
pub(crate) enum Reference {
    Owned(mesh::Mesh),
    Shared {
        mesh: Arc<Mesh>,
        _ticket: BudgetTicket,
        allocation: mesh::Allocation,
    },
}
impl From<mesh::Mesh> for Reference {
    fn from(mesh: mesh::Mesh) -> Self {
        Self::Owned(mesh)
    }
}
impl std::ops::Deref for Reference {
    type Target = mesh::Mesh;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Owned(mesh) => mesh,
            Self::Shared { mesh, .. } => &mesh.mesh,
        }
    }
}
impl Reference {
    pub fn scratch_allocation(&self) -> mesh::Allocation {
        match self {
            Self::Owned(mesh) => mesh.scratch_allocation(),
            Self::Shared { allocation, .. } => allocation.fresh(),
        }
    }
}
pub(crate) struct Scope<'a> {
    pub budgets: &'a Budgets,
    pub limit: u64,
    pub enabled: bool,
}
impl Cache<ShapeDependency, Arc<Mesh>> {
    pub fn shape(
        &mut self,
        frame: &FrameDrawing,
        origin: CommandOrigin,
        geometry: (
            &reel_ui::imaging::record::Geometry,
            reel_ui::peniko::Fill,
            Option<&reel_ui::kurbo::Stroke>,
        ),
        dependency: ShapeDependency,
        allocation: mesh::Allocation,
        scope: Scope<'_>,
    ) -> Result<Reference, GpuError> {
        if let Some(cached) = self.get(frame, origin, &dependency)
            && let Ok(mesh) = cached.value().borrow(allocation.fresh())
        {
            return Ok(mesh);
        }
        let admit =
            scope.enabled && self.observe(frame, origin, &dependency, scope.budgets, scope.limit);
        let (shape, fill, stroke) = geometry;
        let mut mesh = mesh::tessellate(shape, fill, stroke, origin.at, allocation)?;
        mesh.set_image_sampling(dependency.sampling);
        mesh::transform(
            &mut mesh,
            dependency.transform,
            dependency.size,
            dependency.uv,
            dependency.color,
            origin.at,
        )?;
        if admit
            && frame.recording(origin).is_some()
            && let Some(cached) = Mesh::copy(&mut mesh, scope.budgets, scope.limit)
        {
            self.insert(
                frame,
                origin,
                dependency,
                cached,
                scope.budgets,
                scope.limit,
            );
        }
        Ok(mesh.into())
    }
}
