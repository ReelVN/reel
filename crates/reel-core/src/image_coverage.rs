//! Bounded conservative alpha coverage for immutable pictures, independent of
//! author coordinates and device texture padding. No pixel storage is retained.

use crate::texture::{TextureDescriptor, TextureFormat};

/// Maximum horizontal bands retained for one immutable image.
pub const IMAGE_COVERAGE_BANDS: usize = 16;

/// Conservative nonzero-alpha support in visible texels. Each band retains one
/// horizontal interval, including a one-texel filter border. The fixed metadata
/// is computed once from decoded pixels or existing BC blocks; file formats and
/// author layout dimensions remain unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageCoverage {
    size: [u32; 2],
    vertical: [u32; 2],
    spans: [[u32; 2]; IMAGE_COVERAGE_BANDS],
}

impl ImageCoverage {
    fn empty(size: [u32; 2]) -> Self {
        Self {
            size,
            vertical: [size[1], 0],
            spans: [[size[0], 0]; IMAGE_COVERAGE_BANDS],
        }
    }

    fn band_height(&self) -> u32 {
        self.size[1].div_ceil(IMAGE_COVERAGE_BANDS as u32)
    }

    // Expand both axes before assigning bands: filtering near a band boundary
    // can read the neighbouring row. Bands themselves never overlap.
    fn include(&mut self, [x0, y0, x1, y1]: [u32; 4]) {
        let x0 = x0.saturating_sub(1);
        let y0 = y0.saturating_sub(1);
        let x1 = x1.saturating_add(1).min(self.size[0]);
        let y1 = y1.saturating_add(1).min(self.size[1]);
        self.vertical[0] = self.vertical[0].min(y0);
        self.vertical[1] = self.vertical[1].max(y1);
        let step = self.band_height();
        for band in y0 / step..=(y1 - 1) / step {
            let span = &mut self.spans[band as usize];
            span[0] = span[0].min(x0);
            span[1] = span[1].max(x1);
        }
    }

    /// Inspect tightly packed RGBA8 or BGRA8 pixels. Alpha is the fourth byte;
    /// hidden RGB at zero alpha is ignored by the decoded-image upload contract.
    /// Invalid extents return `None`, allowing the caller's ordinary path.
    pub fn rgba8(size: [u32; 2], bytes: &[u8]) -> Option<Self> {
        Self::uncompressed(
            size,
            usize::try_from(u64::from(size[0]) * 4).ok()?,
            bytes,
            true,
        )
    }

    fn uncompressed(size: [u32; 2], stride: usize, bytes: &[u8], alpha_only: bool) -> Option<Self> {
        if size.contains(&0)
            || usize::try_from(u64::from(size[0]) * 4).ok()? > stride
            || u64::from(size[1] - 1)
                .checked_mul(stride as u64)?
                .checked_add(u64::from(size[0]) * 4)?
                > bytes.len() as u64
        {
            return None;
        }
        let mut coverage = Self::empty(size);
        for y in 0..size[1] {
            let start = y as usize * stride;
            let row = &bytes[start..start + size[0] as usize * 4];
            // Opaque/common rows inspect only their two edge pixels. Sparse
            // rows scan inward; interior holes are deliberately enclosed.
            let active = |pixel: &[u8]| {
                if alpha_only {
                    pixel[3] != 0
                } else {
                    pixel != [0; 4]
                }
            };
            if let Some(first) = row.chunks_exact(4).position(active) {
                let last = row.chunks_exact(4).rposition(active)?;
                coverage.include([first as u32, y, last as u32 + 1, y + 1]);
            }
        }
        Some(coverage)
    }

    /// Inspect validated portable prepared storage without decompressing BC
    /// blocks to pixels. BC3 retains whole active 4x4 blocks, conservatively
    /// enclosing every possible hardware alpha interpolation. BC1 is opaque;
    /// unsupported storage returns `None` and retains the ordinary geometry.
    pub fn texture(descriptor: TextureDescriptor, bytes: &[u8]) -> Option<Self> {
        descriptor.validate(bytes.len()).ok()?;
        let size = descriptor.layout.content_size;
        let storage = descriptor.layout.storage_size;
        match descriptor.format {
            TextureFormat::Rgba8 | TextureFormat::Bgra8 => {
                Self::uncompressed(size, storage[0] as usize * 4, bytes, false)
            }
            TextureFormat::Bc3Rgba => {
                let stride = storage[0] as usize / 4 * 16;
                let mut coverage = Self::empty(size);
                for y in 0..size[1].div_ceil(4) {
                    let start = y as usize * stride;
                    let row = &bytes[start..start + size[0].div_ceil(4) as usize * 16];
                    if let Some(first) = row.chunks_exact(16).position(bc3_has_alpha) {
                        let last = row.chunks_exact(16).rposition(bc3_has_alpha)?;
                        coverage.include([
                            first as u32 * 4,
                            y * 4,
                            (last as u32 * 4 + 4).min(size[0]),
                            (y * 4 + 4).min(size[1]),
                        ]);
                    }
                }
                Some(coverage)
            }
            TextureFormat::Bc1Rgb | TextureFormat::LinearRgba16Float => None,
        }
    }

    /// Visible texel dimensions used by this coverage, excluding texture padding.
    pub fn size(&self) -> [u32; 2] {
        self.size
    }

    /// Whether every stored sample is known to produce zero color and alpha.
    pub fn is_empty(&self) -> bool {
        self.vertical[0] >= self.vertical[1]
    }

    /// Whether the conservative support already occupies the complete image.
    /// Such an image keeps its original single rectangle without band planning.
    pub fn is_full(&self) -> bool {
        self.vertical == [0, self.size[1]]
            && self.spans[..self.size[1].div_ceil(self.band_height()) as usize]
                .iter()
                .all(|span| *span == [0, self.size[0]])
    }

    /// Nonoverlapping pixel-edge rectangles, with equal neighbouring bands
    /// coalesced. Empty pictures yield no rectangles; full pictures yield one.
    pub fn rectangles(&self) -> impl Iterator<Item = [u32; 4]> + '_ {
        let mut band = 0;
        let step = self.band_height();
        std::iter::from_fn(move || {
            while band < IMAGE_COVERAGE_BANDS {
                let current = band;
                band += 1;
                let [x0, x1] = self.spans[current];
                if x0 >= x1 {
                    continue;
                }
                while band < IMAGE_COVERAGE_BANDS && self.spans[band] == [x0, x1] {
                    band += 1;
                }
                return Some([
                    x0,
                    (current as u32 * step).max(self.vertical[0]),
                    x1,
                    (band as u32).saturating_mul(step).min(self.vertical[1]),
                ]);
            }
            None
        })
    }
}

fn bc3_has_alpha(block: &[u8]) -> bool {
    let [a, b] = [block[0], block[1]];
    let mut codes = u64::from_le_bytes(block[..8].try_into().unwrap()) >> 16;
    for _ in 0..16 {
        let code = codes & 7;
        codes >>= 3;
        let nonzero = match code {
            0 => a != 0,
            1 => b != 0,
            6 if a <= b => false,
            7 if a <= b => true,
            // Every interpolated entry has a positive weight for both ends.
            _ => a != 0 || b != 0,
        };
        if nonzero {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture::{ColorEncoding, TextureLayout};

    #[test]
    fn rgba_bands_include_filter_neighbours_and_coalesce_full_images() {
        let size = [64, 64];
        let mut bytes = vec![0; 64 * 64 * 4];
        let empty = ImageCoverage::rgba8(size, &bytes).unwrap();
        assert_eq!(empty.rectangles().count(), 0);
        // Hidden colors stay invisible on decoded upload.
        bytes[0] = 255;
        assert_eq!(ImageCoverage::rgba8(size, &bytes).unwrap(), empty);
        bytes[(8 * 64 + 20) * 4 + 3] = 1;
        let coverage = ImageCoverage::rgba8(size, &bytes).unwrap();
        assert_eq!(coverage.rectangles().collect::<Vec<_>>(), [[19, 7, 22, 10]]);
        bytes.fill(255);
        let full = ImageCoverage::rgba8(size, &bytes).unwrap();
        assert_eq!(full.rectangles().collect::<Vec<_>>(), [[0, 0, 64, 64]]);
        assert!(ImageCoverage::rgba8([0, 1], &bytes).is_none());
        assert!(ImageCoverage::rgba8(size, &bytes[..bytes.len() - 1]).is_none());
    }

    #[test]
    fn bc3_checks_zero_endpoints_special_selectors_and_excludes_storage_padding() {
        let descriptor = TextureDescriptor {
            format: TextureFormat::Bc3Rgba,
            source_encoding: ColorEncoding::Srgb,
            layout: TextureLayout {
                logical_size: [80, 80],
                content_size: [36, 36],
                storage_size: [64, 64],
            },
        };
        let mut bytes = vec![0; descriptor.byte_len().unwrap() as usize];
        // A nonzero padding block must not enlarge visible coverage.
        bytes[15 * 16] = 255;
        assert_eq!(
            ImageCoverage::texture(descriptor, &bytes)
                .unwrap()
                .rectangles()
                .count(),
            0
        );
        let block = (2 * 16 + 3) * 16;
        // Endpoints are both zero; selector seven still means opaque alpha.
        bytes[block + 2] = 7;
        assert_eq!(
            ImageCoverage::texture(descriptor, &bytes)
                .unwrap()
                .rectangles()
                .collect::<Vec<_>>(),
            [[11, 7, 17, 13]]
        );
        bytes[block + 2] = 6;
        assert_eq!(
            ImageCoverage::texture(descriptor, &bytes)
                .unwrap()
                .rectangles()
                .count(),
            0
        );
        bytes[block] = 1;
        bytes[block + 2] = 2;
        assert!(bc3_has_alpha(&bytes[block..block + 16]));
    }

    #[test]
    #[ignore = "explicit CPU cost measurement; run in release with --nocapture"]
    fn coverage_scan_costs() {
        use std::{hint::black_box, time::Instant};
        let size = [1024, 1024];
        let mut sparse = vec![0; 1024 * 1024 * 4];
        for y in 64..1024usize {
            let half = if y < 512 { 100 } else { 100 + (y - 512) / 2 };
            for x in 512 - half..512 + half {
                sparse[(y * 1024 + x) * 4 + 3] = 255;
            }
        }
        let opaque = vec![255; sparse.len()];
        let descriptor = TextureDescriptor {
            format: TextureFormat::Bc3Rgba,
            source_encoding: ColorEncoding::Srgb,
            layout: TextureLayout::identity(size),
        };
        let mut bc3 = vec![0; descriptor.byte_len().unwrap() as usize];
        for y in 16..256usize {
            let half = if y < 128 { 25 } else { 25 + (y - 128) / 2 };
            for x in 128 - half..128 + half {
                bc3[(y * 256 + x) * 16] = 255;
            }
        }
        for (name, bytes) in [
            ("rgba8_sparse", &sparse),
            ("rgba8_opaque", &opaque),
            ("bc3_sparse", &bc3),
        ] {
            let scan = || {
                if name.starts_with("bc3") {
                    ImageCoverage::texture(descriptor, black_box(bytes)).unwrap()
                } else {
                    ImageCoverage::rgba8(size, black_box(bytes)).unwrap()
                }
            };
            let coverage = scan();
            let rectangles: Vec<_> = coverage.rectangles().collect();
            let area: u64 = rectangles
                .iter()
                .map(|[x0, y0, x1, y1]| u64::from(x1 - x0) * u64::from(y1 - y0))
                .sum();
            let start = Instant::now();
            for _ in 0..100 {
                black_box(scan());
            }
            eprintln!(
                "{name}: scan_us={:.2} metadata_bytes={} quads={} vertices={} indices={} covered_texels={area} original_texels={} reduction_percent={:.2}",
                start.elapsed().as_secs_f64() * 1e4,
                size_of::<ImageCoverage>(),
                rectangles.len(),
                rectangles.len() * 4,
                rectangles.len() * 6,
                u64::from(size[0]) * u64::from(size[1]),
                100. * (1. - area as f64 / (f64::from(size[0]) * f64::from(size[1])))
            );
        }
    }
}
