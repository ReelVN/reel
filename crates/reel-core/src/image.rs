//! Portable decoded picture storage. Producers retain their own allocation leases.
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YuvMatrix {
    Bt601,
    Bt709,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YuvTransfer {
    Bt709,
    Srgb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct YuvColor {
    pub matrix: YuvMatrix,
    pub full_range: bool,
    pub transfer: YuvTransfer,
}

/// The byte arrangement of top-down 8-bit YUV 4:2:0 planes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Yuv420Layout {
    /// Three planes in Y, U, V order, with one byte per texel.
    Planar,
    /// Two planes: Y, then interleaved U/V with two bytes per chroma texel.
    Nv12,
}

/// Immutable, top-down 8-bit planes, including any producer-owned row padding.
/// A producer's Arc keeps decoding/allocation ownership alive until all frames
/// and uploads release it. NV12 consumers access only planes zero and one.
pub trait Yuv420Storage: std::fmt::Debug + Send + Sync {
    /// Borrow one complete plane without transferring its allocation.
    fn plane(&self, index: usize) -> &[u8];
}

#[derive(Clone, Debug)]
pub struct Yuv420Image {
    size: [u32; 2],
    color: YuvColor,
    layout: Yuv420Layout,
    row_strides: [u32; 3],
    byte_len: usize,
    storage: Arc<dyn Yuv420Storage>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageError {
    Dimensions,
    PlaneLength,
    /// A row stride is too short, or an unused NV12 stride is nonzero.
    PlaneStride,
}

impl Yuv420Image {
    /// Validate tightly packed Y, U and V planes without copying their storage.
    pub fn new(
        size: [u32; 2],
        color: YuvColor,
        storage: Arc<dyn Yuv420Storage>,
    ) -> Result<Self, ImageError> {
        let image = Self::with_layout(
            size,
            color,
            Yuv420Layout::Planar,
            [size[0], size[0].div_ceil(2), size[0].div_ceil(2)],
            storage,
        )?;
        for index in 0..image.plane_count() {
            let bytes = u64::from(image.row_bytes(index)) * u64::from(image.plane_size(index)[1]);
            if image.plane(index).len() as u64 != bytes {
                return Err(ImageError::PlaneLength);
            }
        }
        Ok(image)
    }

    /// Validate producer-owned planes with explicit byte strides and layout.
    /// Each stride must cover one visible row; the last row need not include
    /// trailing padding. NV12 uses two planes and requires a zero third stride.
    /// Odd dimensions use ceiling-rounded chroma dimensions in either layout.
    /// Plane spans must fit this host's slice address space.
    pub fn with_layout(
        size: [u32; 2],
        color: YuvColor,
        layout: Yuv420Layout,
        row_strides: [u32; 3],
        storage: Arc<dyn Yuv420Storage>,
    ) -> Result<Self, ImageError> {
        if size.contains(&0) {
            return Err(ImageError::Dimensions);
        }
        let plane_count = match layout {
            Yuv420Layout::Planar => 3,
            Yuv420Layout::Nv12 => {
                if row_strides[2] != 0 {
                    return Err(ImageError::PlaneStride);
                }
                2
            }
        };
        let mut spans = [0usize; 3];
        for (index, &stride) in row_strides.iter().enumerate().take(plane_count) {
            let [width, height] = Self::dimensions(size, index);
            let row_bytes = width
                .checked_mul(if layout == Yuv420Layout::Nv12 && index == 1 {
                    2
                } else {
                    1
                })
                .ok_or(ImageError::Dimensions)?;
            if stride < row_bytes {
                return Err(ImageError::PlaneStride);
            }
            spans[index] = u64::from(height - 1)
                .checked_mul(u64::from(stride))
                .and_then(|n| n.checked_add(u64::from(row_bytes)))
                .and_then(|n| usize::try_from(n).ok())
                .filter(|&n| n <= isize::MAX as usize)
                .ok_or(ImageError::Dimensions)?;
        }
        let mut byte_len = 0usize;
        for (index, &span) in spans.iter().enumerate().take(plane_count) {
            let len = storage.plane(index).len();
            if len < span {
                return Err(ImageError::PlaneLength);
            }
            byte_len = byte_len.checked_add(len).ok_or(ImageError::Dimensions)?;
        }
        Ok(Self {
            size,
            color,
            layout,
            row_strides,
            byte_len,
            storage,
        })
    }

    fn dimensions(size: [u32; 2], index: usize) -> [u32; 2] {
        if index == 0 {
            size
        } else {
            size.map(|n| n.div_ceil(2))
        }
    }

    pub fn size(&self) -> [u32; 2] {
        self.size
    }
    pub fn color(&self) -> YuvColor {
        self.color
    }
    /// The producer's chroma arrangement, retained through resource sharing.
    pub fn layout(&self) -> Yuv420Layout {
        self.layout
    }
    /// Number of addressable planes: three for planar YUV, two for NV12.
    pub fn plane_count(&self) -> usize {
        match self.layout {
            Yuv420Layout::Planar => 3,
            Yuv420Layout::Nv12 => 2,
        }
    }
    /// Visible texture dimensions in texels, excluding row padding.
    /// The index must be smaller than [`Self::plane_count`].
    pub fn plane_size(&self, index: usize) -> [u32; 2] {
        Self::dimensions(self.size, index)
    }
    /// Byte distance between adjacent rows in the producer's plane.
    /// The index must be smaller than [`Self::plane_count`].
    pub fn row_stride(&self, index: usize) -> u32 {
        self.row_strides[index]
    }
    /// Visible bytes per row: two per chroma texel in the NV12 UV plane.
    /// The index must be smaller than [`Self::plane_count`].
    pub fn row_bytes(&self, index: usize) -> u32 {
        self.plane_size(index)[0]
            * if self.layout == Yuv420Layout::Nv12 && index == 1 {
                2
            } else {
                1
            }
    }
    /// Borrow a producer plane, including any retained row padding.
    /// The index must be smaller than [`Self::plane_count`].
    pub fn plane(&self, index: usize) -> &[u8] {
        self.storage.plane(index)
    }
    /// Total bytes exposed by the retained planes, including row padding.
    /// The producer retains responsibility for its actual allocation permits.
    pub fn byte_len(&self) -> usize {
        self.byte_len
    }
    pub fn same_storage(&self, other: &Self) -> bool {
        self.size == other.size
            && self.color == other.color
            && self.layout == other.layout
            && self.row_strides == other.row_strides
            && Arc::ptr_eq(&self.storage, &other.storage)
    }

    /// Three rows mapping normalized Y/U/V samples to nonlinear R/G/B.
    /// The renderer applies the declared transfer curve into its linear target.
    pub fn rgb_rows(&self) -> [[f32; 4]; 3] {
        self.color.rgb_rows()
    }
}

impl YuvColor {
    pub fn rgb_rows(self) -> [[f32; 4]; 3] {
        let (kr, kb) = match self.matrix {
            YuvMatrix::Bt601 => (0.299, 0.114),
            YuvMatrix::Bt709 => (0.2126, 0.0722),
        };
        let kg = 1. - kr - kb;
        let (y_scale, y_offset, c_scale) = if self.full_range {
            (1., 0., 1.)
        } else {
            (255. / 219., -16. / 219., 255. / 224.)
        };
        let c_offset = -128. / 255.;
        let row = |u: f32, v: f32| {
            [
                y_scale,
                u * c_scale,
                v * c_scale,
                y_offset + (u + v) * c_scale * c_offset,
            ]
        };
        [
            row(0., 2. * (1. - kr)),
            row(-2. * kb * (1. - kb) / kg, -2. * kr * (1. - kr) / kg),
            row(2. * (1. - kb), 0.),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug)]
    struct Planes([Vec<u8>; 3]);
    impl Yuv420Storage for Planes {
        fn plane(&self, i: usize) -> &[u8] {
            &self.0[i]
        }
    }

    #[test]
    fn packed_planes_and_declared_color_matrix_are_validated_without_copying() {
        let storage = Arc::new(Planes([vec![16; 9], vec![128; 4], vec![128; 4]]));
        let color = YuvColor {
            matrix: YuvMatrix::Bt709,
            full_range: false,
            transfer: YuvTransfer::Bt709,
        };
        let image = Yuv420Image::new([3, 3], color, storage.clone()).unwrap();
        assert_eq!(image.plane_size(1), [2, 2]);
        assert_eq!(image.byte_len(), 17);
        assert_eq!(image.plane(0).as_ptr(), storage.0[0].as_ptr());
        assert!(image.same_storage(&image.clone()));
        assert!(matches!(
            Yuv420Image::new([3, 2], color, storage.clone()),
            Err(ImageError::PlaneLength)
        ));
        assert!(matches!(
            Yuv420Image::new([0, 2], color, storage),
            Err(ImageError::Dimensions)
        ));
        let convert = |color: YuvColor, yuv: [f32; 3]| {
            color.rgb_rows().map(|row| {
                row[0] * yuv[0] / 255. + row[1] * yuv[1] / 255. + row[2] * yuv[2] / 255. + row[3]
            })
        };
        for matrix in [YuvMatrix::Bt601, YuvMatrix::Bt709] {
            for full_range in [false, true] {
                let color = YuvColor {
                    matrix,
                    full_range,
                    ..color
                };
                let [black, white] = if full_range { [0., 255.] } else { [16., 235.] };
                assert!(
                    convert(color, [black, 128., 128.])
                        .into_iter()
                        .all(|n| n.abs() < 0.000001)
                );
                assert!(
                    convert(color, [white, 128., 128.])
                        .into_iter()
                        .all(|n| (n - 1.).abs() < 0.000001)
                );
            }
        }
        let red = convert(color, [63., 102., 240.]);
        assert!(red[0] > 0.99 && red[1].abs() < 0.01 && red[2].abs() < 0.01);
    }

    #[test]
    fn nv12_retains_producer_strides_and_checks_last_visible_row() {
        // Accessing a third plane would panic: NV12 must keep the native UV pair.
        #[derive(Debug)]
        struct Nv12([Vec<u8>; 2]);
        impl Yuv420Storage for Nv12 {
            fn plane(&self, i: usize) -> &[u8] {
                &self.0[i]
            }
        }
        let storage = Arc::new(Nv12([vec![16; 13], vec![128; 10]]));
        let color = YuvColor {
            matrix: YuvMatrix::Bt601,
            full_range: false,
            transfer: YuvTransfer::Bt709,
        };
        let image = Yuv420Image::with_layout(
            [3, 3],
            color,
            Yuv420Layout::Nv12,
            [5, 6, 0],
            storage.clone(),
        )
        .unwrap();
        assert_eq!(image.plane_count(), 2);
        assert_eq!(image.plane_size(1), [2, 2]);
        assert_eq!(image.row_bytes(1), 4);
        assert_eq!(image.row_stride(1), 6);
        assert_eq!(image.byte_len(), 23);
        assert_eq!(image.plane(1).as_ptr(), storage.0[1].as_ptr());
        assert!(image.same_storage(&image.clone()));
        let different_stride = Yuv420Image::with_layout(
            [3, 3],
            color,
            Yuv420Layout::Nv12,
            [4, 5, 0],
            storage.clone(),
        )
        .unwrap();
        assert!(!image.same_storage(&different_stride));
        for (size, strides, error) in [
            ([3, 3], [5, 3, 0], ImageError::PlaneStride),
            ([3, 3], [5, 6, 1], ImageError::PlaneStride),
            ([3, 3], [6, 6, 0], ImageError::PlaneLength),
            (
                [u32::MAX, 1],
                [u32::MAX, u32::MAX, 0],
                ImageError::Dimensions,
            ),
            (
                [2, u32::MAX],
                [u32::MAX, u32::MAX, 0],
                ImageError::Dimensions,
            ),
            (
                [3, u32::MAX],
                [5, 6, 0],
                if usize::BITS == 32 {
                    ImageError::Dimensions
                } else {
                    ImageError::PlaneLength
                },
            ),
            ([0, 3], [5, 6, 0], ImageError::Dimensions),
        ] {
            assert_eq!(
                Yuv420Image::with_layout(size, color, Yuv420Layout::Nv12, strides, storage.clone())
                    .unwrap_err(),
                error
            );
        }
    }
}
