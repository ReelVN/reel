//! A decoded sRGB image with straight alpha, stored for direct player admission.

/// Dimensions of the tightly packed RGBA8 pixels following a REELRAS1 header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RasterHeader {
    size: [u32; 2],
    byte_len: u64,
}

impl RasterHeader {
    /// Identifies prepared raster files independently of their logical path.
    pub const MAGIC: &[u8; 8] = b"REELRAS1";
    /// Magic and two little-endian u32 dimensions.
    pub const LEN: usize = 16;

    /// Construct a nonempty raster with a representable pixel extent.
    pub fn new(size: [u32; 2]) -> Option<Self> {
        let [width, height] = size.map(u64::from);
        let byte_len = (width * height).checked_mul(4)?;
        (byte_len != 0).then_some(Self { size, byte_len })
    }

    /// The original pixel size and logical image dimensions.
    pub fn size(self) -> [u32; 2] {
        self.size
    }

    /// Number of bytes in the tightly packed pixel payload.
    pub fn byte_len(self) -> u64 {
        self.byte_len
    }

    /// Encode the fixed header. The caller supplies the pixel bytes.
    pub fn encode(self) -> [u8; Self::LEN] {
        let mut bytes = [0; Self::LEN];
        bytes[..8].copy_from_slice(Self::MAGIC);
        bytes[8..12].copy_from_slice(&self.size[0].to_le_bytes());
        bytes[12..16].copy_from_slice(&self.size[1].to_le_bytes());
        bytes
    }

    /// Decode only the fixed-size metadata.
    pub fn decode(header: &[u8; Self::LEN]) -> Option<Self> {
        if !header.starts_with(Self::MAGIC) {
            return None;
        }
        let words = header[8..].as_chunks::<4>().0;
        Self::new([u32::from_le_bytes(words[0]), u32::from_le_bytes(words[1])])
    }

    /// Decode a standalone prepared raster and validate its exact pixel extent.
    pub fn parse(bytes: &[u8]) -> Option<(Self, &[u8])> {
        let (header, pixels) = bytes.split_first_chunk::<{ Self::LEN }>()?;
        let header = Self::decode(header)?;
        (pixels.len() as u64 == header.byte_len).then_some((header, pixels))
    }
}
