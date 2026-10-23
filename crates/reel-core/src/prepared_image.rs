//! Immutable prepared-image format shared by author tooling and resource consumers.
//!
//! Format v1 is a 24-byte little-endian manifest: `REELIMG1`, followed by four
//! `u32` values for original width, original height, interior tile edge and
//! level count. Dimensions and tile edge are positive. Each level halves both
//! dimensions, rounding up, until the pyramid reaches 1 by 1.
//!
//! A manifest named `panorama.reel-image` uses adjacent tile files at
//! `panorama.reel-image.tiles/N/X-Y.png`. Tiles include one neighboring pixel
//! on every edge, clamped at image boundaries. The tile dimensions are the
//! interior dimensions plus two gutter pixels, which must fit the device's
//! texture limit. Preparation filters linear premultiplied colors and stores
//! ordinary sRGB PNGs.
//!
//! Tooling decodes the original offline. Runtime consumers select levels and
//! tiles from the composed viewport and physical scale, sharing resource leases
//! between nodes. Required tiles hold presentation until ready; optional
//! prefetch yields to required resources. Accepted and pending frames retain
//! their actual leases. Tile interiors paint into the shared target, with the
//! gutter providing bilinear samples across boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparedImage {
    pub width: u32,
    pub height: u32,
    pub tile_edge: u32,
}

impl PreparedImage {
    pub fn new(width: u32, height: u32, tile_edge: u32) -> Result<Self, &'static str> {
        let tile_bytes = tile_edge
            .checked_add(2)
            .map(u64::from)
            .and_then(|edge| edge.checked_mul(edge))
            .and_then(|pixels| pixels.checked_mul(4));
        if width == 0
            || height == 0
            || tile_edge == 0
            || tile_bytes.is_none_or(|bytes| usize::try_from(bytes).is_err())
        {
            return Err(
                "prepared image dimensions and tile edge must be positive and representable",
            );
        }
        Ok(Self {
            width,
            height,
            tile_edge,
        })
    }
    pub fn levels(self) -> u32 {
        33 - (self.width.max(self.height) - 1).leading_zeros()
    }
    pub fn dimensions(self, level: u32) -> [u32; 2] {
        let divisor = 1_u64 << level;
        [
            u64::from(self.width).div_ceil(divisor) as u32,
            u64::from(self.height).div_ceil(divisor) as u32,
        ]
    }
    pub fn encode(self) -> [u8; 24] {
        let mut bytes = [0; 24];
        bytes[..8].copy_from_slice(b"REELIMG1");
        for (chunk, value) in bytes[8..].chunks_exact_mut(4).zip([
            self.width,
            self.height,
            self.tile_edge,
            self.levels(),
        ]) {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() != 24 || &bytes[..8] != b"REELIMG1" {
            return Err("expected a 24-byte REELIMG1 manifest");
        }
        let mut values = bytes[8..]
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let image = Self::new(
            values.next().unwrap(),
            values.next().unwrap(),
            values.next().unwrap(),
        )?;
        if values.next() != Some(image.levels()) {
            return Err("prepared image pyramid level count is invalid");
        }
        Ok(image)
    }
    pub fn tile_dimensions(self, level: u32, x: u32, y: u32) -> Option<[u32; 2]> {
        if level >= self.levels() {
            return None;
        }
        let [width, height] = self.dimensions(level);
        let left = x.checked_mul(self.tile_edge)?;
        let top = y.checked_mul(self.tile_edge)?;
        if left >= width || top >= height {
            return None;
        }
        Some([
            (width - left).min(self.tile_edge) + 2,
            (height - top).min(self.tile_edge) + 2,
        ])
    }
    pub fn tile_source(source: &str, level: u32, x: u32, y: u32) -> String {
        format!("{source}.tiles/{level}/{x}-{y}.png")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifest_round_trip_and_odd_pyramid_edges() {
        let image = PreparedImage::new(40001, 17, 512).unwrap();
        assert_eq!(PreparedImage::decode(&image.encode()), Ok(image));
        assert_eq!(image.dimensions(1), [20001, 9]);
        assert_eq!(image.dimensions(image.levels() - 1), [1, 1]);
        assert_eq!(image.tile_dimensions(0, 78, 0), Some([67, 19]));
        assert_eq!(image.tile_dimensions(0, 79, 0), None);
        let mut invalid = image.encode();
        invalid[20] = 0;
        assert!(PreparedImage::decode(&invalid).is_err());
        assert!(PreparedImage::decode(&invalid[..23]).is_err());
        assert!(PreparedImage::new(0, 17, 512).is_err());
    }
}
