//! Immutable prepared-image format shared by author tooling and resource consumers.
//!
//! Format v1 is a 32-byte little-endian manifest: `REELIMG1`, followed by six
//! `u32` values for original width, original height, interior tile edge and
//! level count, tile encoding (0 = PNG, 1 = Reel texture) and first stored level.
//! Dimensions and tile edge are positive. Each level halves both dimensions,
//! rounding up, until the pyramid reaches 1 by 1. Only levels starting at the
//! first stored level have tile files; the original dimensions remain logical.
//!
//! A manifest named `panorama.reel-image` uses adjacent tile files at
//! `panorama.reel-image.tiles/N/X-Y.png` or `X-Y.reel-texture`, according to the
//! manifest encoding. Tiles include one neighboring pixel
//! on every edge, clamped at image boundaries. The tile dimensions are the
//! interior dimensions plus two gutter pixels, which must fit the device's
//! texture limit. Preparation filters linear premultiplied colors. PNG stores
//! sRGB samples; Reel textures store linear premultiplied texels or BC blocks.
//!
//! Tooling decodes the original offline. Runtime consumers select levels and
//! tiles from the composed viewport and physical scale, sharing resource leases
//! between nodes. Required tiles hold presentation until ready; optional
//! prefetch yields to required resources. Accepted and pending frames retain
//! their actual leases. Tile interiors paint into the shared target, with the
//! gutter providing bilinear samples across boundaries.
/// The encoding used by every tile in one prepared image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TileEncoding {
    /// Ordinary sRGB PNG artwork.
    Png = 0,
    /// A validated Reel texture, including native compressed BC storage.
    ReelTexture = 1,
}

/// Original author coordinates and storage selection for one tile pyramid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparedImage {
    /// Full-resolution author width, before pyramid downsampling.
    pub width: u32,
    /// Full-resolution author height, before pyramid downsampling.
    pub height: u32,
    /// Interior tile edge, excluding each tile's one-pixel gutter.
    pub tile_edge: u32,
    /// File encoding shared by all levels and tiles.
    pub tile_encoding: TileEncoding,
    /// Lowest stored pyramid level. Author dimensions retain full resolution.
    pub first_level: u32,
}

impl PreparedImage {
    /// Content signature used independently of the author's logical filename.
    pub const MAGIC: &'static [u8; 8] = b"REELIMG1";
    /// Validate author dimensions and the maximum decoded tile allocation.
    pub fn new(
        width: u32,
        height: u32,
        tile_edge: u32,
        tile_encoding: TileEncoding,
    ) -> Result<Self, &'static str> {
        let tile_bytes = tile_edge
            .checked_add(2)
            .map(u64::from)
            .and_then(|edge| edge.checked_mul(edge))
            .and_then(|pixels| pixels.checked_mul(4));
        if width == 0
            || height == 0
            || tile_edge == 0
            || tile_bytes.is_none_or(|bytes| bytes > isize::MAX as u64)
        {
            return Err(
                "prepared image dimensions and tile edge must be positive and representable",
            );
        }
        Ok(Self {
            width,
            height,
            tile_edge,
            tile_encoding,
            first_level: 0,
        })
    }
    /// Select the first stored level while preserving the original author size.
    pub fn with_first_level(mut self, first_level: u32) -> Result<Self, &'static str> {
        if first_level >= self.levels() {
            return Err("prepared image first stored level is outside the pyramid");
        }
        self.first_level = first_level;
        Ok(self)
    }
    /// Number of levels, including the original resolution and final 1x1 level.
    pub fn levels(self) -> u32 {
        33 - (self.width.max(self.height) - 1).leading_zeros()
    }
    /// Texel dimensions of a valid pyramid level, rounding each axis up.
    pub fn dimensions(self, level: u32) -> [u32; 2] {
        let divisor = 1_u64 << level;
        [
            u64::from(self.width).div_ceil(divisor) as u32,
            u64::from(self.height).div_ceil(divisor) as u32,
        ]
    }
    /// Serialize the complete v1 manifest.
    pub fn encode(self) -> [u8; 32] {
        assert!(
            self.first_level < self.levels(),
            "first stored level must belong to the pyramid"
        );
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(Self::MAGIC);
        for (chunk, value) in bytes[8..].chunks_exact_mut(4).zip([
            self.width,
            self.height,
            self.tile_edge,
            self.levels(),
            self.tile_encoding as u32,
            self.first_level,
        ]) {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }
    /// Parse the exact v1 layout and validate its dimensions and encoding.
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() != 32 || &bytes[..8] != Self::MAGIC {
            return Err("expected a 32-byte REELIMG1 manifest");
        }
        let mut values = bytes[8..]
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let width = values.next().unwrap();
        let height = values.next().unwrap();
        let tile_edge = values.next().unwrap();
        let levels = values.next().unwrap();
        let encoding = match values.next().unwrap() {
            0 => TileEncoding::Png,
            1 => TileEncoding::ReelTexture,
            _ => return Err("prepared image tile encoding is invalid"),
        };
        let image = Self::new(width, height, tile_edge, encoding)?
            .with_first_level(values.next().unwrap())?;
        if levels != image.levels() {
            return Err("prepared image pyramid level count is invalid");
        }
        Ok(image)
    }
    /// Logical tile dimensions including gutters, or `None` outside stored levels.
    pub fn tile_dimensions(self, level: u32, x: u32, y: u32) -> Option<[u32; 2]> {
        if level < self.first_level || level >= self.levels() {
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
    /// Adjacent resource path selected by this manifest's tile encoding.
    pub fn tile_source(self, source: &str, level: u32, x: u32, y: u32) -> String {
        let extension = match self.tile_encoding {
            TileEncoding::Png => "png",
            TileEncoding::ReelTexture => "reel-texture",
        };
        format!("{source}.tiles/{level}/{x}-{y}.{extension}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifest_round_trip_and_odd_pyramid_edges() {
        let image = PreparedImage::new(40001, 17, 512, TileEncoding::ReelTexture).unwrap();
        assert_eq!(PreparedImage::decode(&image.encode()), Ok(image));
        assert_eq!(image.dimensions(1), [20001, 9]);
        assert_eq!(image.dimensions(image.levels() - 1), [1, 1]);
        assert_eq!(image.tile_dimensions(0, 78, 0), Some([67, 19]));
        assert_eq!(image.tile_dimensions(0, 79, 0), None);
        let mut invalid = image.encode();
        invalid[20] = 0;
        assert!(PreparedImage::decode(&invalid).is_err());
        assert!(PreparedImage::decode(&invalid[..23]).is_err());
        assert!(PreparedImage::new(0, 17, 512, TileEncoding::Png).is_err());
        let mut invalid = image.encode();
        invalid[24] = 2;
        assert!(PreparedImage::decode(&invalid).is_err());
        assert_eq!(
            image.tile_source("panorama.reel-image", 0, 2, 3),
            "panorama.reel-image.tiles/0/2-3.reel-texture"
        );
        let png = PreparedImage::new(17, 17, 8, TileEncoding::Png).unwrap();
        assert_eq!(PreparedImage::decode(&png.encode()), Ok(png));
        assert!(png.tile_source("x", 0, 0, 0).ends_with(".png"));
    }
    #[test]
    fn first_stored_level_preserves_author_size_and_rejects_missing_levels() {
        let image = PreparedImage::new(1920, 29616, 510, TileEncoding::ReelTexture)
            .unwrap()
            .with_first_level(1)
            .unwrap();
        assert_eq!(image.encode().len(), 32);
        assert_eq!(PreparedImage::decode(&image.encode()), Ok(image));
        assert_eq!([image.width, image.height], [1920, 29616]);
        assert_eq!(image.dimensions(1), [960, 14808]);
        assert_eq!(image.tile_dimensions(0, 0, 0), None);
        assert_eq!(image.tile_dimensions(1, 0, 0), Some([512, 512]));
        assert!(image.with_first_level(image.levels()).is_err());
        assert!(PreparedImage::decode(&image.encode()[..28]).is_err());
        let mut invalid = image.encode();
        invalid[28..32].copy_from_slice(&image.levels().to_le_bytes());
        assert!(PreparedImage::decode(&invalid).is_err());
        let invalid = PreparedImage {
            first_level: image.levels(),
            ..image
        };
        assert!(std::panic::catch_unwind(|| invalid.encode()).is_err());
    }
}
