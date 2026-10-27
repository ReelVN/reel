//! Choose one element width for an ordered geometry segment on both backends.
use super::{GpuError, mesh::Mesh};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    U16,
    U32,
}

pub(crate) enum Upload<'a, R> {
    Narrow { values: Vec<u16>, _reservation: R },
    Wide(&'a [u32]),
}

impl<'a, R> Upload<'a, R> {
    /// Mesh construction validates the vertex domain. Padding preserves WGPU's
    /// four-byte copy alignment; recorded draw ranges exclude that final zero.
    pub(crate) fn new(
        mesh: &'a Mesh,
        reserve: impl FnOnce(u64) -> Result<R, GpuError>,
    ) -> Result<Self, GpuError> {
        if mesh.vertices.len() > usize::from(u16::MAX) + 1 {
            return Ok(Self::Wide(&mesh.indices));
        }
        let len = mesh.indices.len().div_ceil(2) * 2;
        let reservation = reserve((len * size_of::<u16>()) as u64)?;
        let mut values = Vec::with_capacity(len);
        values.extend(mesh.indices.iter().map(|&index| {
            debug_assert!(index <= u32::from(u16::MAX));
            index as u16
        }));
        values.resize(len, 0);
        Ok(Self::Narrow {
            values,
            _reservation: reservation,
        })
    }
    pub(crate) fn format(&self) -> Format {
        match self {
            Self::Narrow { .. } => Format::U16,
            Self::Wide(_) => Format::U32,
        }
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Self::Narrow { values, .. } => bytemuck::cast_slice(values),
            Self::Wide(values) => bytemuck::cast_slice(values),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{
        mesh::{Allocation, Vertex},
        renderer::Budgets,
    };

    #[test]
    fn width_boundary_alignment_and_index_order_are_preserved() {
        let budgets = Budgets::new(crate::common::GpuLimits::default());
        let mut mesh = Mesh::new(Allocation::new(&budgets, &budgets, u64::MAX, u64::MAX));
        mesh.vertices.resize(
            65536,
            Vertex {
                position: [0.; 2],
                uv: [0.; 2],
                color: [1.; 4],
                source_window: [0.; 4],
            },
        );
        mesh.indices.extend([65535, 7, 0]);
        let packed = Upload::new(&mesh, |bytes| {
            assert_eq!(bytes, 8);
            Ok(())
        })
        .unwrap();
        assert_eq!(packed.format(), Format::U16);
        assert_eq!(
            bytemuck::cast_slice::<u8, u16>(packed.bytes()),
            &[65535, 7, 0, 0]
        );
        drop(packed);
        mesh.vertices.push(mesh.vertices[0]);
        mesh.indices[0] = 65536;
        let packed = Upload::<()>::new(&mesh, |_| {
            panic!("wide indices borrow their original storage")
        })
        .unwrap();
        assert_eq!(packed.format(), Format::U32);
        assert_eq!(packed.bytes().as_ptr(), mesh.indices.as_ptr().cast());
        assert_eq!(
            bytemuck::cast_slice::<u8, u32>(packed.bytes()),
            &[65536, 7, 0]
        );
    }
}
