//! Ordered geometry segments share an upload pair, with per-draw state intact.
use super::{GpuError, mesh::Mesh};
use std::ops::Range;

pub(crate) struct Entry<T> {
    pub range: Range<u32>,
    pub state: T,
}
pub(crate) struct Batch<T> {
    pub mesh: Mesh,
    pub entries: Vec<Entry<T>>,
}
impl<T> Batch<T> {
    /// Transfer the first mesh and its capacity tickets without copying geometry.
    pub fn new(mut mesh: Mesh, state: T) -> Result<Self, GpuError> {
        let end = u32::try_from(mesh.indices.len()).map_err(|_| GpuError::InvalidSize)?;
        let mut entries = Vec::new();
        mesh.reserve_entries(&mut entries)?;
        entries.push(Entry {
            range: 0..end,
            state,
        });
        Ok(Self { mesh, entries })
    }
    /// A flush threshold, not a limit on a single authored mesh.
    pub fn full(&self, next: &Mesh) -> bool {
        self.entries.len() >= 64 || self.mesh.bytes().saturating_add(next.bytes()) > 256 * 1024
    }
    pub fn push(&mut self, mesh: Mesh, state: T) -> Result<(), GpuError> {
        self.mesh.reserve_entries(&mut self.entries)?;
        let range = self.mesh.append(&mesh)?;
        self.entries.push(Entry { range, state });
        Ok(())
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
        assert_eq!(batch.mesh.vertices.as_ptr(), vertex_storage);
        assert_eq!(batch.mesh.indices.as_ptr(), index_storage);
        batch.push(second, (2, [10, 20, 50, 60])).unwrap();
        assert_eq!(batch.entries[0].range, 0..6);
        assert_eq!(batch.entries[1].range, 6..12);
        assert_eq!(batch.entries[0].state, (1, [3, 4, 20, 30]));
        assert_eq!(batch.entries[1].state, (2, [10, 20, 50, 60]));
        for (entry, original) in batch.entries.iter().zip(expected) {
            for (combined, original) in batch.mesh.indices
                [entry.range.start as usize..entry.range.end as usize]
                .iter()
                .zip(original)
            {
                assert_eq!(
                    bytemuck::bytes_of(&batch.mesh.vertices[*combined as usize]),
                    original
                );
            }
        }
        drop(batch);
        assert_eq!(shared.transient.usage().bytes, 0);
        assert_eq!(local.transient.usage().bytes, 0);
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
        assert_eq!(batch.mesh.vertices.as_ptr(), vertex_storage);
        assert_eq!(batch.mesh.indices.as_ptr(), index_storage);
        assert!(batch.full(&Mesh::new(allocation())));
        assert!(batch.mesh.indices.iter().copied().max().unwrap() > u32::from(u16::MAX));
    }
}
