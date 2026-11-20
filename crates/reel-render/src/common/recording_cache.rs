//! Bounded immutable geometry keyed by a live source recording.
use super::{GpuError, mesh, renderer::Budgets};
use hashbrown::HashTable;
use reel_core::budget::{BudgetTicket, collections::reserve_table};
use reel_ui::frame::{CommandOrigin, FrameDrawing, LocalId, RecordingSource};
use reel_ui::kurbo::Affine;
use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
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
impl<P: Clone, G: Clone> Glyphs<P, G> {
    pub fn new(budgets: &Budgets, limit: u64) -> Option<Self> {
        let ticket = admit(budgets, limit, 0)?;
        Some(Self {
            segments: Vec::new(),
            leases: Vec::new(),
            _ticket: ticket,
        })
    }
    pub fn lease(capture: &mut Option<Self>, id: u32, glyph: &G, budgets: &Budgets, limit: u64) {
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
        value.leases.insert(index, (id, glyph.clone()));
    }
    pub fn capture(
        capture: &mut Option<Self>,
        mesh: &mut mesh::Mesh,
        page: &P,
        budgets: &Budgets,
        limit: u64,
    ) {
        let Some(value) = capture else {
            return;
        };
        let Some(cached) = Mesh::copy(mesh, budgets, limit) else {
            *capture = None;
            return;
        };
        if grow(&mut value.segments, &mut value._ticket, budgets, limit).is_none() {
            *capture = None;
            return;
        }
        value.segments.push((cached, page.clone()));
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
    hash: u64,
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
    index: HashTable<u8>,
    index_storage: Option<BudgetTicket>,
    next: usize,
    lookup: usize,
    located: Option<usize>,
    high_water: usize,
}
impl<D, T> Default for Cache<D, T> {
    fn default() -> Self {
        Self {
            entries: std::array::from_fn(|_| None),
            index: HashTable::new(),
            index_storage: None,
            next: 0,
            lookup: 0,
            located: None,
            high_water: 0,
        }
    }
}

fn recording_hash(source: &RecordingSource, local: LocalId) -> u64 {
    BuildHasherDefault::<DefaultHasher>::default().hash_one((source, local))
}

fn remove_index(index: &mut HashTable<u8>, hash: u64, slot: u8) {
    index
        .find_entry(hash, |&candidate| candidate == slot)
        .expect("resident geometry has an index slot")
        .remove();
}

impl<D: PartialEq + Clone, T: StorageSize> Cache<D, T> {
    // Replay visits recordings in order even when their transforms change.
    // Keep the ordered cursor for the common case, and probe the bounded
    // index when recordings are reordered or a new one appears mid-frame.
    fn locate(&mut self, source: &RecordingSource, local: LocalId) -> Option<usize> {
        let matches = |index: usize| {
            self.entries[index]
                .as_ref()
                .is_some_and(|entry| entry.source == *source && entry.local == local)
        };
        let index = if matches(self.lookup) {
            Some(self.lookup)
        } else {
            self.located.filter(|&index| matches(index)).or_else(|| {
                self.index
                    .find(recording_hash(source, local), |&slot| matches(slot.into()))
                    .map(|&slot| usize::from(slot))
            })
        };
        self.located = index;
        if let Some(index) = index {
            self.lookup = (index + 1) % self.entries.len();
        }
        index
    }
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
        if self
            .locate(source, local)
            .is_some_and(|index| self.entries[index].as_ref().unwrap().dependency == *dependency)
        {
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
        let Some((source, local)) = frame.recording(origin) else {
            self.begin_replay();
            return None;
        };
        let index = self.locate(source, local)?;
        let entry = self.entries[index].as_ref().unwrap();
        (entry.dependency == *dependency && entry.value.is_some()).then(|| entry.clone())
    }
    /// Retire expired recordings once before replay, including entries outside
    /// the next draw's search prefix. Frame/flight references keep their payloads.
    pub(crate) fn begin_replay(&mut self) {
        for (slot, entry) in self.entries[..self.high_water].iter_mut().enumerate() {
            if let Some(current) = entry.as_ref()
                && !current.source.is_live()
            {
                remove_index(&mut self.index, current.hash, slot as u8);
                *entry = None;
            }
        }
        while self.high_water != 0 && self.entries[self.high_water - 1].is_none() {
            self.high_water -= 1;
        }
        if self.index.is_empty() {
            self.index = HashTable::new();
            self.index_storage = None;
        }
        self.lookup = 0;
        self.located = None;
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
        let existing = self.locate(source, local);
        if let Some(index) = existing
            && let Some(entry) = self.entries[index].as_mut()
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
        let index = existing.unwrap_or(self.next);
        let hash = recording_hash(source, local);
        let new_slot = if existing.is_none() {
            let entries = &self.entries;
            let slot_hash = |&slot: &u8| entries[usize::from(slot)].as_ref().unwrap().hash;
            if self.index_storage.is_none()
                && reserve_table(
                    &mut self.index,
                    entries.len(),
                    &mut self.index_storage,
                    &budgets.transient,
                    slot_hash,
                )
                .is_err()
            {
                return;
            }
            let slot = index as u8;
            if let Some(old) = &entries[index] {
                remove_index(&mut self.index, old.hash, slot);
            }
            // After prolonged replacements, deleted buckets can exhaust the
            // insertion capacity. Reuse the charged allocation and erase
            // those buckets before publishing the new slot.
            if self.index.len() == self.index.capacity() {
                self.index.clear();
                for (existing_slot, entry) in entries.iter().enumerate() {
                    if let Some(entry) = entry
                        && existing_slot != index
                    {
                        self.index
                            .insert_unique(entry.hash, existing_slot as u8, slot_hash);
                    }
                }
            }
            Some(slot)
        } else {
            None
        };
        self.high_water = self.high_water.max(index + 1);
        self.entries[index] = Some(Arc::new(Entry {
            source: source.clone(),
            local,
            hash,
            dependency,
            value,
            _ticket: ticket,
        }));
        if let Some(slot) = new_slot {
            let entries = &self.entries;
            self.index.insert_unique(hash, slot, |&slot| {
                entries[usize::from(slot)].as_ref().unwrap().hash
            });
        }
        self.located = Some(index);
        self.lookup = (index + 1) % self.entries.len();
        if index == self.next {
            self.next = (self.next + 1) % self.entries.len();
        }
    }
    pub fn clear(&mut self) {
        self.entries.fill_with(|| None);
        self.index = HashTable::new();
        self.index_storage = None;
        self.next = 0;
        self.lookup = 0;
        self.located = None;
        self.high_water = 0;
    }
    pub fn bytes(&self) -> u64 {
        self.entries
            .iter()
            .flatten()
            .map(|entry| {
                entry._ticket.bytes() + entry.value.as_ref().map_or(0, |value| value.bytes())
            })
            .sum::<u64>()
            + self.index_storage.as_ref().map_or(0, BudgetTicket::bytes)
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
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::GeometryPrepare);
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

#[cfg(test)]
mod tests;
