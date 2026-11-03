//! Pack retained segments directly into the consumer's upload storage.
use super::Segment;
use crate::common::{geometry::Capacity, indices::Format, mesh::Vertex};

pub(crate) struct View<'a> {
    segments: &'a [Segment],
    first_vertex: usize,
    format: Format,
}

impl<'a> View<'a> {
    pub(super) fn new(segments: &'a [Segment], first_vertex: usize, format: Format) -> Self {
        Self {
            segments,
            first_vertex,
            format,
        }
    }

    pub fn capacity(&self) -> Capacity {
        let vertices = self.segments.iter().map(|s| s.key.vertices).sum::<usize>();
        let indices = self.segments.iter().map(|s| s.key.indices).sum::<usize>();
        Capacity {
            vertices: (vertices * size_of::<Vertex>()) as u64,
            indices: (indices * self.width()).div_ceil(4) as u64 * 4,
            index_format: self.format,
        }
    }

    fn width(&self) -> usize {
        match self.format {
            Format::U16 => 2,
            Format::U32 => 4,
        }
    }
    #[cfg(feature = "wgpu")]
    pub fn index_bytes(&self) -> usize {
        self.segments.iter().map(|s| s.key.indices).sum::<usize>() * self.width()
    }

    #[cfg(any(feature = "wgpu", test))]
    pub fn vertices(&self, mut write: impl FnMut(usize, &[u8])) {
        let mut offset = 0;
        for segment in self.segments {
            let bytes = bytemuck::cast_slice(&segment.mesh.vertices);
            write(offset, bytes);
            offset += bytes.len();
        }
    }

    /// Rebase once and write bounded chunks. Offsets are relative to this view;
    /// index values remain relative to the complete batch's vertex buffer.
    /// The caller owns copy alignment, including a final unused half-word.
    pub fn indices(&self, mut write: impl FnMut(usize, &[u8])) {
        let mut packed = [0_u8; 1024];
        let (mut used, mut offset, mut vertex) = (0, 0, self.first_vertex as u32);
        let width = self.width();
        for segment in self.segments {
            for &index in &segment.mesh.indices {
                let index = vertex + index;
                if width == 2 {
                    debug_assert!(index <= u32::from(u16::MAX));
                    packed[used..used + 2].copy_from_slice(&(index as u16).to_le_bytes());
                } else {
                    packed[used..used + 4].copy_from_slice(&index.to_le_bytes());
                }
                used += width;
                if used == packed.len() {
                    write(offset, &packed);
                    offset += used;
                    used = 0;
                }
            }
            vertex += segment.key.vertices as u32;
        }
        if used != 0 {
            write(offset, &packed[..used]);
        }
    }

    /// GXM copies CPU storage into mapped native memory. A single segment keeps
    /// its original vertices; merged segments allocate only the final layout.
    #[cfg(any(feature = "gxm", test))]
    pub fn pack<R>(
        &self,
        reserve: impl FnOnce(u64) -> Result<R, crate::common::GpuError>,
    ) -> Result<Packed<'a, R>, crate::common::GpuError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::GeometryPack);
        let capacity = self.capacity();
        let single = self.segments.len() == 1;
        let reservation = reserve(capacity.indices + if single { 0 } else { capacity.vertices })?;
        let vertices = if single {
            std::borrow::Cow::Borrowed(self.segments[0].mesh.vertices.as_slice())
        } else {
            let mut vertices = Vec::with_capacity(capacity.vertices as usize / size_of::<Vertex>());
            for segment in self.segments {
                vertices.extend_from_slice(&segment.mesh.vertices);
            }
            std::borrow::Cow::Owned(vertices)
        };
        let mut indices = vec![0_u32; capacity.indices as usize / 4];
        let bytes = bytemuck::cast_slice_mut(&mut indices);
        self.indices(|offset, source| bytes[offset..offset + source.len()].copy_from_slice(source));
        Ok(Packed {
            vertices,
            indices,
            format: self.format,
            _reservation: reservation,
        })
    }
}

#[cfg(any(feature = "gxm", test))]
pub(crate) struct Packed<'a, R> {
    pub vertices: std::borrow::Cow<'a, [Vertex]>,
    pub indices: Vec<u32>,
    pub format: Format,
    _reservation: R,
}
