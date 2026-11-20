//! Bounded conservative alpha coverage for immutable pictures, independent of
//! author coordinates and device texture padding. No pixel storage is retained.

use crate::texture::{TextureDescriptor, TextureFormat};

/// Maximum horizontal bands retained for one immutable image.
pub const IMAGE_COVERAGE_BANDS: usize = 16;

const ALPHA_BLOCK: u128 = 0xff00_0000_ff00_0000_ff00_0000_ff00_0000;

/// Conservative nonzero-alpha support in visible texels. Each band retains one
/// horizontal interval, including a one-texel filter border. The fixed metadata
/// is computed once from decoded pixels or existing BC blocks; file formats and
/// author layout dimensions remain unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageCoverage {
    size: [u32; 2],
    vertical: [u32; 2],
    spans: [[u32; 2]; IMAGE_COVERAGE_BANDS],
    opaque: bool,
}

impl ImageCoverage {
    fn empty(size: [u32; 2]) -> Self {
        Self {
            size,
            vertical: [size[1], 0],
            spans: [[size[0], 0]; IMAGE_COVERAGE_BANDS],
            opaque: false,
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
        let _timing = crate::profile::Scope::new(crate::profile::Phase::ImageCoverage);
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
        coverage.opaque = true;
        let active_mask = if alpha_only { ALPHA_BLOCK } else { u128::MAX };
        for y in 0..size[1] {
            let start = y as usize * stride;
            let row = &bytes[start..start + size[0] as usize * 4];
            let (blocks, tail) = row.as_chunks::<16>();
            let tail = tail.as_chunks::<4>().0;
            if coverage.opaque
                && blocks
                    .iter()
                    .all(|block| u128::from_le_bytes(*block) & ALPHA_BLOCK == ALPHA_BLOCK)
                && tail.iter().all(|pixel| pixel[3] == 255)
            {
                coverage.include([0, y, size[0], y + 1]);
                continue;
            }
            coverage.opaque = false;
            if let Some([first, end]) = active_span(blocks, tail, active_mask) {
                coverage.include([first, y, end, y + 1]);
            }
        }
        Some(coverage)
    }

    /// Inspect validated portable prepared storage without decompressing BC
    /// blocks to pixels. BC3 retains whole active 4x4 blocks, conservatively
    /// enclosing every possible hardware alpha interpolation. BC1 is opaque;
    /// unsupported storage returns `None` and retains the ordinary geometry.
    pub fn texture(descriptor: TextureDescriptor, bytes: &[u8]) -> Option<Self> {
        let _timing = crate::profile::Scope::new(crate::profile::Phase::ImageCoverage);
        // Coarse mip filtering can spread alpha beyond the base-level coverage bands.
        if descriptor.mip_count > 1 {
            return None;
        }
        descriptor.validate(bytes.len()).ok()?;
        let size = descriptor.layout.content_size;
        let storage = descriptor.layout.storage_size;
        match descriptor.format {
            TextureFormat::Rgba8 | TextureFormat::Bgra8 => {
                Self::uncompressed(size, storage[0] as usize * 4, bytes, false)
            }
            TextureFormat::Indexed8Rgba => {
                let palette = bytes[descriptor.palette_offset()? as usize..]
                    .as_chunks::<4>()
                    .0;
                if palette.iter().all(|color| color[3] == 255) {
                    let mut coverage = Self::empty(size);
                    coverage.include([0, 0, size[0], size[1]]);
                    coverage.opaque = true;
                    return Some(coverage);
                }
                let samples: [u8; 256] = std::array::from_fn(|i| match palette[i] {
                    [_, _, _, 255] => 2,
                    [0, 0, 0, 0] => 0,
                    _ => 1,
                });
                let mut coverage = Self::empty(size);
                coverage.opaque = true;
                let active = |&index: &u8| samples[index as usize] != 0;
                for y in 0..size[1] {
                    let start = y as usize * storage[0] as usize;
                    let row = &bytes[start..start + size[0] as usize];
                    if coverage.opaque && row.iter().all(|&index| samples[index as usize] == 2) {
                        coverage.include([0, y, size[0], y + 1]);
                        continue;
                    }
                    coverage.opaque = false;
                    if let Some(first) = row.iter().position(active) {
                        let last = row.iter().rposition(active).unwrap();
                        coverage.include([first as u32, y, last as u32 + 1, y + 1]);
                    }
                }
                Some(coverage)
            }
            TextureFormat::Bc3Rgba => {
                let mut coverage = Self::empty(size);
                for y in 0..size[1].div_ceil(4) {
                    let active = |x: u32| {
                        let at = descriptor.storage_layout.block_offset(storage, 16, x, y);
                        bc3_has_alpha(bytes[at..at + 16].try_into().unwrap())
                    };
                    if let Some(first) = (0..size[0].div_ceil(4)).find(|&x| active(x)) {
                        let last = (first..size[0].div_ceil(4)).rfind(|&x| active(x))?;
                        coverage.include([
                            first * 4,
                            y * 4,
                            (last * 4 + 4).min(size[0]),
                            (y * 4 + 4).min(size[1]),
                        ]);
                    }
                }
                Some(coverage)
            }
            TextureFormat::Bc1Rgb | TextureFormat::Rgb8 | TextureFormat::LinearRgba16Float => None,
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

    /// Whether every visible texel has alpha one. Unlike `is_full`, this proves
    /// interior alpha as well as the image's conservative support bounds.
    pub fn is_opaque(&self) -> bool {
        self.opaque
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

fn active_span(blocks: &[[u8; 16]], tail: &[[u8; 4]], mask: u128) -> Option<[u32; 2]> {
    // Four little-endian pixels share one test. A hit still resolves to the
    // exact first or last pixel within the block.
    let active_block = |block: &[u8; 16]| u128::from_le_bytes(*block) & mask;
    let pixel_mask = mask as u32;
    let active_pixel = |pixel: &[u8; 4]| u32::from_le_bytes(*pixel) & pixel_mask != 0;
    let tail_start = blocks.len() as u32 * 4;
    let first = blocks
        .iter()
        .map(active_block)
        .enumerate()
        .find(|(_, bits)| *bits != 0)
        .map(|(block, bits)| block as u32 * 4 + bits.trailing_zeros() / 32)
        .or_else(|| {
            tail.iter()
                .position(active_pixel)
                .map(|pixel| tail_start + pixel as u32)
        })?;
    let end = tail
        .iter()
        .rposition(active_pixel)
        .map(|pixel| tail_start + pixel as u32 + 1)
        .or_else(|| {
            blocks
                .iter()
                .map(active_block)
                .enumerate()
                .rfind(|(_, bits)| *bits != 0)
                .map(|(block, bits)| block as u32 * 4 + (128 - bits.leading_zeros()).div_ceil(32))
        })?;
    Some([first, end])
}

fn bc3_has_alpha(block: &[u8; 16]) -> bool {
    let [a, b] = [block[0], block[1]];
    if a > b && b != 0 {
        return true;
    }
    let codes = u64::from_le_bytes(block[..8].try_into().unwrap()) >> 16;
    const SELECTOR_LOW_BITS: u64 = ((1 << 48) - 1) / 7;
    match (a, b) {
        // With both endpoints zero, only selector 7 is active.
        (0, 0) => codes & (codes >> 1) & (codes >> 2) & SELECTOR_LOW_BITS != 0,
        // Selectors 0 and 6 are the zero values. All other selectors are
        // odd, or differ between their two high bits.
        (0, _) => (codes | ((codes >> 1) ^ (codes >> 2))) & SELECTOR_LOW_BITS != 0,
        // These modes each have one zero selector. Test the packed value
        // against that selector repeated across the whole block.
        (_, 0) => codes != SELECTOR_LOW_BITS,
        _ => codes != 6 * SELECTOR_LOW_BITS,
    }
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
        assert!(full.is_opaque());
        bytes[(32 * 64 + 32) * 4 + 3] = 254;
        let hole = ImageCoverage::rgba8(size, &bytes).unwrap();
        assert!(hole.is_full());
        assert!(!hole.is_opaque());
        assert!(ImageCoverage::rgba8([0, 1], &bytes).is_none());
        assert!(ImageCoverage::rgba8(size, &bytes[..bytes.len() - 1]).is_none());
    }

    #[test]
    fn uncompressed_blocks_resolve_exact_extents_and_odd_row_tails() {
        for width in 1u32..=19 {
            let size = [width, 1];
            let mut bytes = vec![0; width as usize * 4];
            for x in 0..width {
                for channel in [0, 3] {
                    let position = x as usize * 4 + channel;
                    bytes[position] = 1;
                    let outline = [x.saturating_sub(1), 0, (x + 2).min(width), 1];
                    let raw =
                        ImageCoverage::uncompressed(size, bytes.len(), &bytes, false).unwrap();
                    assert_eq!(raw.rectangles().collect::<Vec<_>>(), [outline]);
                    let decoded = ImageCoverage::rgba8(size, &bytes).unwrap();
                    if channel == 3 {
                        assert_eq!(decoded.rectangles().collect::<Vec<_>>(), [outline]);
                    } else {
                        assert!(decoded.is_empty());
                    }
                    bytes[position] = 0;
                }
            }
        }
    }

    #[test]
    fn bc3_checks_zero_endpoints_special_selectors_and_excludes_storage_padding() {
        let descriptor = TextureDescriptor {
            storage_layout: crate::texture::TextureStorageLayout::Linear,
            mip_count: 1,
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
        assert!(bc3_has_alpha(bytes[block..block + 16].try_into().unwrap()));
    }

    #[test]
    fn bc3_packed_selectors_preserve_every_alpha_mode() {
        let cases = [
            (0, 0, &[0, 1, 2, 3, 4, 5, 6][..]),
            (0, 150, &[0, 6][..]),
            (150, 0, &[1][..]),
            (150, 150, &[6][..]),
            (1, 150, &[6][..]),
            (150, 1, &[][..]),
        ];
        for (a, b, zeros) in cases {
            let mut block = [0; 16];
            block[..2].copy_from_slice(&[a, b]);
            for background in 0..8u64 {
                let uniform =
                    (0..16).fold(0, |bits, position| bits | (background << (3 * position)));
                for position in 0..16 {
                    for selector in 0..8u64 {
                        let shift = 3 * position;
                        let bits = (uniform & !(7 << shift)) | (selector << shift);
                        block[2..8].copy_from_slice(&bits.to_le_bytes()[..6]);
                        let active = !zeros.contains(&background) || !zeros.contains(&selector);
                        assert_eq!(bc3_has_alpha(&block), active);
                    }
                }
            }
        }
    }
}
