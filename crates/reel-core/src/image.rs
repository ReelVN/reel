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

/// Immutable, top-down packed 8-bit Y, U and V planes. A producer's Arc keeps
/// decoding/allocation ownership alive until all frames and uploads release it.
pub trait Yuv420Storage: std::fmt::Debug + Send + Sync {
    fn plane(&self, index: usize) -> &[u8];
}

#[derive(Clone, Debug)]
pub struct Yuv420Image {
    size: [u32; 2],
    color: YuvColor,
    storage: Arc<dyn Yuv420Storage>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageError {
    Dimensions,
    PlaneLength,
}

impl Yuv420Image {
    /// Validate one decoded-frame boundary without copying its planes.
    pub fn new(
        size: [u32; 2],
        color: YuvColor,
        storage: Arc<dyn Yuv420Storage>,
    ) -> Result<Self, ImageError> {
        if size.contains(&0) {
            return Err(ImageError::Dimensions);
        }
        for index in 0..3 {
            let plane = Self::dimensions(size, index);
            let bytes = (plane[0] as usize)
                .checked_mul(plane[1] as usize)
                .ok_or(ImageError::Dimensions)?;
            if storage.plane(index).len() != bytes {
                return Err(ImageError::PlaneLength);
            }
        }
        Ok(Self {
            size,
            color,
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
    pub fn plane_size(&self, index: usize) -> [u32; 2] {
        Self::dimensions(self.size, index)
    }
    pub fn plane(&self, index: usize) -> &[u8] {
        self.storage.plane(index)
    }
    pub fn byte_len(&self) -> usize {
        (0..3).map(|i| self.plane(i).len()).sum()
    }
    pub fn same_storage(&self, other: &Self) -> bool {
        self.size == other.size
            && self.color == other.color
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
}
