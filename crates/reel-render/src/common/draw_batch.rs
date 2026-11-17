//! Ordered immutable geometry segments retain the existing adjacent batching.
use super::{
    GpuError,
    mesh::{Allocation, Mesh, RectClip},
    recording_cache::Reference,
};
#[cfg(test)]
use std::ops::Deref;
use std::ops::Range;
pub(crate) const MAX_BYTES: usize = 256 * 1024;

mod packing;
#[cfg(feature = "gxm")]
pub(crate) use packing::Packed;
pub(crate) use packing::View;

pub(crate) struct Entry<T> {
    pub range: Range<u32>,
    pub state: T,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SegmentKey {
    pub identity: Option<u64>,
    pub clip: Option<RectClip>,
    pub vertices: usize,
    pub indices: usize,
}
pub(crate) struct Segment {
    pub mesh: Reference,
    pub key: SegmentKey,
}
pub(crate) struct Batch<T> {
    pub entries: Vec<Entry<T>>,
    pub segments: Vec<Segment>,
    vertices: usize,
    indices: usize,
    bytes: usize,
    allocation: Allocation,
}
// This temporary upload view stays on the stack; boxing adds an allocation to
// every cold batch and does not reduce any retained collection.
#[allow(clippy::large_enum_variant)]
#[cfg(test)]
pub(crate) enum Materialized<'a> {
    Borrowed(&'a Mesh),
    Owned(Mesh),
}
#[cfg(test)]
impl Deref for Materialized<'_> {
    type Target = Mesh;
    fn deref(&self) -> &Mesh {
        match self {
            Self::Borrowed(mesh) => mesh,
            Self::Owned(mesh) => mesh,
        }
    }
}
impl<T> Batch<T> {
    pub fn new(mesh: impl Into<Reference>, state: T) -> Result<Self, GpuError> {
        let mesh = mesh.into();
        let allocation = mesh.scratch_allocation();
        let mut batch = Self {
            entries: Vec::new(),
            segments: Vec::new(),
            vertices: 0,
            indices: 0,
            bytes: 0,
            allocation,
        };
        batch.allocation.grow(&mut batch.entries, 1)?;
        let range = batch.append(mesh)?;
        batch.entries.push(Entry { range, state });
        Ok(batch)
    }
    pub fn full(&self, next: &Mesh) -> bool {
        self.full_bytes(next.bytes())
    }
    pub fn full_bytes(&self, bytes: usize) -> bool {
        self.entries.len() >= 64 || self.bytes.saturating_add(bytes) > MAX_BYTES
    }
    fn append(&mut self, mesh: Reference) -> Result<Range<u32>, GpuError> {
        let end = self
            .indices
            .checked_add(mesh.indices.len())
            .filter(|&n| n <= i32::MAX as usize)
            .ok_or(GpuError::InvalidSize)?;
        let vertices = self
            .vertices
            .checked_add(mesh.vertices.len())
            .filter(|&n| n <= u32::MAX as usize)
            .ok_or(GpuError::InvalidSize)?;
        self.allocation.grow(&mut self.segments, 1)?;
        let key = SegmentKey {
            identity: mesh.identity,
            clip: mesh.clip_identity,
            vertices: mesh.vertices.len(),
            indices: mesh.indices.len(),
        };
        self.bytes += mesh.bytes();
        self.segments.push(Segment { mesh, key });
        let range = self.indices as u32..end as u32;
        self.vertices = vertices;
        self.indices = end;
        Ok(range)
    }
    pub fn push(&mut self, mesh: impl Into<Reference>, state: T) -> Result<(), GpuError>
    where
        T: PartialEq,
    {
        let merge = self.entries.last().is_some_and(|last| last.state == state);
        if !merge {
            self.allocation.grow(&mut self.entries, 1)?;
        }
        let range = self.append(mesh.into())?;
        if merge {
            self.entries.last_mut().unwrap().range.end = range.end;
        } else {
            self.entries.push(Entry { range, state });
        }
        Ok(())
    }
    #[cfg(test)]
    pub fn materialize(&self) -> Result<Materialized<'_>, GpuError> {
        self.materialize_range(0..self.segments.len())
    }
    #[cfg(test)]
    pub fn materialize_range(&self, range: Range<usize>) -> Result<Materialized<'_>, GpuError> {
        if range.len() == 1 {
            return Ok(Materialized::Borrowed(&self.segments[range.start].mesh));
        }
        let mut mesh = Mesh::new(self.allocation.fresh());
        for segment in &self.segments[range] {
            mesh.append(&segment.mesh)?;
        }
        Ok(Materialized::Owned(mesh))
    }
    pub fn view(&self, range: UploadRange) -> View<'_> {
        View::new(
            &self.segments[range.segments],
            range.vertices.start,
            self.capacity().index_format,
        )
    }
    pub fn full_view(&self) -> View<'_> {
        View::new(&self.segments, 0, self.capacity().index_format)
    }
    pub fn capacity(&self) -> super::geometry::Capacity {
        let narrow = self.vertices <= usize::from(u16::MAX) + 1;
        super::geometry::Capacity {
            vertices: (self.vertices * size_of::<super::mesh::Vertex>()) as u64,
            indices: if narrow {
                (self.indices.div_ceil(2) * 4) as u64
            } else {
                (self.indices * 4) as u64
            },
            index_format: if narrow {
                super::indices::Format::U16
            } else {
                super::indices::Format::U32
            },
        }
    }
}

/// Completed native buffers retain only segment identities and extents. The
/// identities refer to immutable CPU meshes, so no source or atlas owner escapes.
pub(crate) struct Layout {
    keys: Vec<SegmentKey>,
    ticket: reel_core::budget::BudgetTicket,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UploadRange {
    pub segments: Range<usize>,
    pub vertices: Range<usize>,
    pub indices: Range<usize>,
}
impl Layout {
    /// Adjacent dirty segments become one transfer range, independent of draw
    /// state boundaries. Byte alignment is applied after this common planning.
    pub fn ranges<'a, T>(
        &'a self,
        batch: &'a Batch<T>,
    ) -> impl Iterator<Item = UploadRange> + Clone + 'a {
        let (mut next, mut vertex, mut index) = (0, 0, 0);
        std::iter::from_fn(move || {
            while next < batch.segments.len() {
                let segment = &batch.segments[next];
                if self.matches(next, segment.key) {
                    vertex += segment.key.vertices;
                    index += segment.key.indices;
                    next += 1;
                    continue;
                }
                let (start, first_vertex, first_index) = (next, vertex, index);
                while next < batch.segments.len() && !self.matches(next, batch.segments[next].key) {
                    vertex += batch.segments[next].key.vertices;
                    index += batch.segments[next].key.indices;
                    next += 1;
                }
                return Some(UploadRange {
                    segments: start..next,
                    vertices: first_vertex..vertex,
                    indices: first_index..index,
                });
            }
            None
        })
    }
    pub fn bytes(&self) -> u64 {
        self.ticket.bytes()
    }
    pub fn compatible<T>(&self, batch: &Batch<T>) -> bool {
        self.keys.len() == batch.segments.len()
            && self.keys.iter().zip(&batch.segments).all(|(old, next)| {
                old.vertices == next.key.vertices && old.indices == next.key.indices
            })
    }
    pub fn matches(&self, index: usize, key: SegmentKey) -> bool {
        key.identity.is_some() && self.keys.get(index) == Some(&key)
    }
    pub fn score<T>(&self, batch: &Batch<T>) -> usize {
        if !self.compatible(batch) {
            return 0;
        }
        batch
            .segments
            .iter()
            .enumerate()
            .filter(|(i, s)| self.matches(*i, s.key))
            .map(|(_, s)| s.mesh.bytes())
            .sum()
    }
    pub fn record<T>(
        layout: &mut Option<Self>,
        batch: &Batch<T>,
        budgets: &super::renderer::Budgets,
        local: &super::renderer::Budgets,
    ) -> Option<reel_core::budget::BudgetTicket> {
        if let Some(layout) = layout
            && layout.compatible(batch)
        {
            for (key, segment) in layout.keys.iter_mut().zip(&batch.segments) {
                *key = segment.key;
            }
            return None;
        }
        *layout = None;
        let bytes = (batch.segments.len() * size_of::<SegmentKey>()) as u64;
        let local = local.transient.reserve(bytes).ok()?;
        let ticket = budgets.transient.reserve(bytes).ok()?;
        *layout = Some(Self {
            keys: batch.segments.iter().map(|segment| segment.key).collect(),
            ticket,
        });
        Some(local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{GpuLimits, mesh::Allocation, renderer::Budgets};
    use reel_ui::kurbo::Rect;

    #[test]
    fn ranges_preserve_primitive_order_attributes_and_state() {
        let limits = GpuLimits::default();
        let shared = Budgets::new(limits);
        let local = Budgets::new(limits);
        let allocation = || Allocation::new(&shared, &local, limits.transient_bytes, 1 << 26);
        let mut first = Mesh::new(allocation());
        first
            .quad(Rect::new(1., 2., 3., 4.), Rect::new(0.2, 0.3, 0.6, 0.8))
            .unwrap();
        for vertex in &mut first.vertices {
            vertex.color = [0.1, 0.2, 0.3, 0.4];
            vertex.source_window = [0.2, 0.1, -0.3, 0.7];
        }
        let mut second = Mesh::new(allocation());
        second
            .quad(Rect::new(-4., -3., 2., 1.), Rect::ZERO)
            .unwrap();
        let expected = [&first, &second].map(|mesh| {
            mesh.indices
                .iter()
                .map(|&index| bytemuck::bytes_of(&mesh.vertices[index as usize]).to_vec())
                .collect::<Vec<_>>()
        });
        let vertex_storage = first.vertices.as_ptr();
        let index_storage = first.indices.as_ptr();
        let mut batch = Batch::new(first, (1, [3, 4, 20, 30])).unwrap();
        assert_eq!(
            batch.materialize().unwrap().vertices.as_ptr(),
            vertex_storage
        );
        assert_eq!(batch.materialize().unwrap().indices.as_ptr(), index_storage);
        batch.push(second, (2, [10, 20, 50, 60])).unwrap();
        assert_eq!(batch.entries[0].range, 0..6);
        assert_eq!(batch.entries[1].range, 6..12);
        assert_eq!(batch.entries[0].state, (1, [3, 4, 20, 30]));
        assert_eq!(batch.entries[1].state, (2, [10, 20, 50, 60]));
        for (entry, original) in batch.entries.iter().zip(expected) {
            for (combined, original) in batch.materialize().unwrap().indices
                [entry.range.start as usize..entry.range.end as usize]
                .iter()
                .zip(original)
            {
                assert_eq!(
                    bytemuck::bytes_of(&batch.materialize().unwrap().vertices[*combined as usize]),
                    original
                );
            }
        }
        drop(batch);
        assert_eq!(shared.transient.usage().bytes, 0);
        assert_eq!(local.transient.usage().bytes, 0);
    }

    #[test]
    fn adjacent_equal_states_merge_without_reordering_across_a_change() {
        let limits = GpuLimits::default();
        let shared = Budgets::new(limits);
        let local = Budgets::new(limits);
        let mesh = || {
            let mut mesh = Mesh::new(Allocation::new(
                &shared,
                &local,
                limits.transient_bytes,
                1 << 26,
            ));
            mesh.quad(Rect::new(0., 0., 8., 8.), Rect::ZERO).unwrap();
            mesh
        };
        let mut batch = Batch::new(mesh(), 1).unwrap();
        batch.push(mesh(), 1).unwrap();
        batch.push(mesh(), 2).unwrap();
        batch.push(mesh(), 1).unwrap();
        assert_eq!(
            batch
                .entries
                .iter()
                .map(|e| (e.state, e.range.clone()))
                .collect::<Vec<_>>(),
            vec![(1, 0..12), (2, 12..18), (1, 18..24)]
        );
        assert_eq!(batch.materialize().unwrap().indices.len(), 24);
    }

    #[test]
    fn large_mesh_keeps_u32_indices_and_is_a_flush_boundary() {
        let limits = GpuLimits::default();
        let shared = Budgets::new(limits);
        let local = Budgets::new(limits);
        let allocation = || Allocation::new(&shared, &local, limits.transient_bytes, 1 << 26);
        let mut mesh = Mesh::new(allocation());
        for _ in 0..16_385 {
            mesh.quad(Rect::new(0., 0., 1., 1.), Rect::ZERO).unwrap();
        }
        let vertex_storage = mesh.vertices.as_ptr();
        let index_storage = mesh.indices.as_ptr();
        let batch = Batch::new(mesh, ()).unwrap();
        assert_eq!(
            batch.materialize().unwrap().vertices.as_ptr(),
            vertex_storage
        );
        assert_eq!(batch.materialize().unwrap().indices.as_ptr(), index_storage);
        assert!(batch.full(&Mesh::new(allocation())));
        assert!(
            batch
                .materialize()
                .unwrap()
                .indices
                .iter()
                .copied()
                .max()
                .unwrap()
                > u32::from(u16::MAX)
        );
    }

    #[test]
    fn changed_ranges_coalesce_without_changing_adjacent_draw_batches() {
        let limits = GpuLimits::default();
        let shared = Budgets::new(limits);
        let local = Budgets::new(limits);
        let make = |ids: [u64; 5]| {
            let mut batch = None;
            for id in ids {
                let mut mesh = Mesh::new(Allocation::new(
                    &shared,
                    &local,
                    limits.transient_bytes,
                    1 << 26,
                ));
                mesh.quad(Rect::new(0., 0., 1., 1.), Rect::ZERO).unwrap();
                mesh.indices.truncate(3);
                mesh.identity = Some(id);
                if let Some(batch) = &mut batch {
                    Batch::push(batch, mesh, 1).unwrap();
                } else {
                    batch = Some(Batch::new(mesh, 1).unwrap());
                }
            }
            batch.unwrap()
        };
        let initial = make([1, 2, 3, 4, 5]);
        let mut layout = None;
        let _local = Layout::record(&mut layout, &initial, &shared, &local).unwrap();
        let layout = layout.unwrap();
        assert_eq!(layout.ranges(&initial).count(), 0);
        let changed = make([1, 6, 7, 4, 8]);
        assert_eq!(changed.entries.len(), 1);
        assert_eq!(changed.entries[0].range, 0..15);
        assert!(layout.compatible(&changed));
        assert_eq!(
            layout.ranges(&changed).collect::<Vec<_>>(),
            vec![
                UploadRange {
                    segments: 1..3,
                    vertices: 4..12,
                    indices: 3..9
                },
                UploadRange {
                    segments: 4..5,
                    vertices: 16..20,
                    indices: 12..15
                },
            ]
        );
        let middle = changed.materialize_range(1..3).unwrap();
        assert_eq!(middle.indices, [0, 1, 2, 4, 5, 6]);
        let packed = changed
            .view(layout.ranges(&changed).next().unwrap())
            .pack(|_| Ok(()))
            .unwrap();
        assert_eq!(
            bytemuck::cast_slice::<u32, u16>(&packed.indices),
            [4, 5, 6, 8, 9, 10]
        );
        assert_eq!(packed.format, super::super::indices::Format::U16);
        let mut unknown = make([1, 2, 3, 4, 5]);
        for segment in &mut unknown.segments {
            segment.key.identity = None;
        }
        assert_eq!(
            layout.score(&unknown),
            0,
            "a full upload is selected without retained segments"
        );
    }

    #[test]
    fn packed_upload_preserves_vertices_indices_alignment_and_reservation() {
        use crate::common::{
            indices::{Format, Upload},
            mesh::Vertex,
        };
        let limits = GpuLimits::default();
        let shared = Budgets::new(limits);
        let local = Budgets::new(limits);
        for count in [1, 2, 65_536, 65_537] {
            let mut mesh = Mesh::new(Allocation::new(
                &shared,
                &local,
                limits.transient_bytes,
                1 << 26,
            ));
            mesh.vertices.resize(
                count,
                Vertex {
                    position: [0.5, -0.5],
                    uv: [0.2, 0.7],
                    color: [0.1, 0.2, 0.3, 0.4],
                    source_window: [0.2, 0.3, 0.6, 0.7],
                },
            );
            mesh.indices.extend([count as u32 - 1, 0, count as u32 - 1]);
            let mut batch = Batch::new(mesh, ()).unwrap();
            for merged in [false, true] {
                if merged {
                    let mut mesh = Mesh::new(Allocation::new(
                        &shared,
                        &local,
                        limits.transient_bytes,
                        1 << 26,
                    ));
                    mesh.quad(Rect::new(3., 4., 5., 6.), Rect::ZERO).unwrap();
                    batch.push(mesh, ()).unwrap();
                }
                let old = batch.materialize().unwrap();
                let indices = Upload::new(&old, |_| Ok(())).unwrap();
                let view = batch.full_view();
                let bytes = view.capacity();
                let packed = view
                    .pack(|requested| {
                        assert_eq!(
                            requested,
                            bytes.indices + if merged { bytes.vertices } else { 0 }
                        );
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(
                    bytemuck::cast_slice::<_, u8>(&packed.vertices),
                    bytemuck::cast_slice::<_, u8>(&old.vertices)
                );
                assert_eq!(
                    bytemuck::cast_slice::<_, u8>(&packed.indices),
                    indices.bytes()
                );
                if !merged {
                    assert_eq!(
                        packed.vertices.as_ptr(),
                        batch.segments[0].mesh.vertices.as_ptr()
                    );
                }
                assert_eq!(
                    packed.format,
                    if old.vertices.len() > 65_536 {
                        Format::U32
                    } else {
                        Format::U16
                    }
                );
                assert!(view.pack::<()>(|_| Err(GpuError::InvalidSize)).is_err());
            }
        }
    }
}
