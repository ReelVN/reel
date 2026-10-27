//! Portable, linear-premultiplied texture storage and its offline file envelope.
//! BC payloads contain standard little-endian blocks in row-major order. Native
//! device layouts belong to platform adapters, never to this shared format.
use crate::budget::OwnedBytes;
/// Original source-channel transfer function, retained for numeric coverage.
/// Prepared texture storage itself is always linear premultiplied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ColorEncoding {
    /// Source channels use the sRGB transfer function.
    Srgb,
    /// Source channels are linear numeric values.
    Linear,
}

/// Storage units accepted by image consumers. BC1 has opaque RGB semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TextureFormat {
    /// Four linear-premultiplied unsigned normalized channels in RGBA order.
    Rgba8,
    /// Four linear-premultiplied unsigned normalized channels in BGRA order.
    Bgra8,
    /// Four linear-premultiplied little-endian IEEE binary16 channels.
    LinearRgba16Float,
    /// Opaque RGB in standard eight-byte BC1 blocks.
    Bc1Rgb,
    /// Linear-premultiplied RGBA in standard sixteen-byte BC3 blocks.
    Bc3Rgba,
}
impl TextureFormat {
    /// Width and height of one independently stored unit.
    pub const fn block_size(self) -> u32 {
        if self.is_compressed() { 4 } else { 1 }
    }
    /// Bytes in one pixel or compressed block.
    pub const fn block_bytes(self) -> u64 {
        match self {
            Self::Rgba8 | Self::Bgra8 => 4,
            Self::LinearRgba16Float | Self::Bc1Rgb => 8,
            Self::Bc3Rgba => 16,
        }
    }
    /// Compressed formats have no per-pixel byte stride.
    pub const fn pixel_bytes(self) -> Option<u64> {
        if self.is_compressed() {
            None
        } else {
            Some(self.block_bytes())
        }
    }
    /// Whether storage units cover four-by-four texels instead of one pixel.
    pub const fn is_compressed(self) -> bool {
        matches!(self, Self::Bc1Rgb | Self::Bc3Rgba)
    }
    /// Packed bytes per storage block row, before an adapter's copy alignment.
    pub fn row_bytes(self, width: u32) -> Option<u64> {
        u64::from(width)
            .div_ceil(u64::from(self.block_size()))
            .checked_mul(self.block_bytes())
    }
    /// Packed payload extent; checked before converting to address-space sizes.
    pub fn byte_len(self, size: [u32; 2]) -> Option<u64> {
        self.row_bytes(size[0])?
            .checked_mul(u64::from(size[1]).div_ceil(u64::from(self.block_size())))
    }
    fn code(self) -> u32 {
        match self {
            Self::Rgba8 => 0,
            Self::Bgra8 => 1,
            Self::LinearRgba16Float => 2,
            Self::Bc1Rgb => 3,
            Self::Bc3Rgba => 4,
        }
    }
    fn from_code(code: u32) -> Result<Self, TextureError> {
        match code {
            0 => Ok(Self::Rgba8),
            1 => Ok(Self::Bgra8),
            2 => Ok(Self::LinearRgba16Float),
            3 => Ok(Self::Bc1Rgb),
            4 => Ok(Self::Bc3Rgba),
            _ => Err(TextureError::Format),
        }
    }
}
/// Author coordinates, visible texels and allocated texels are independent.
/// Visible content starts at zero; any padding extends its right/bottom edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TextureLayout {
    /// Extent used by author layout, source rectangles, slices and input.
    pub logical_size: [u32; 2],
    /// Filtered, visible texels before block or device padding.
    pub content_size: [u32; 2],
    /// Complete allocated texture extent, including right/bottom edge padding.
    pub storage_size: [u32; 2],
}
impl TextureLayout {
    /// Describe an image with no offline scaling or padding.
    pub const fn identity(size: [u32; 2]) -> Self {
        Self {
            logical_size: size,
            content_size: size,
            storage_size: size,
        }
    }
    /// Convert an authored source pixel to a normalized device texture coordinate.
    pub fn uv_scale(self) -> [f64; 2] {
        std::array::from_fn(|i| {
            f64::from(self.content_size[i])
                / f64::from(self.logical_size[i])
                / f64::from(self.storage_size[i])
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Format, coordinate mapping and source transfer metadata for prepared storage.
pub struct TextureDescriptor {
    /// Native sample storage; all prepared color channels are linear premultiplied.
    pub format: TextureFormat,
    /// Author coordinates and visible/allocated texture extents.
    pub layout: TextureLayout,
    /// Original channel encoding, used only to recover numeric mask/packed-alpha
    /// coverage. It never changes the prepared storage's linear color contract.
    pub source_encoding: ColorEncoding,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// A malformed descriptor, file envelope or payload extent.
pub enum TextureError {
    Header,
    Format,
    Dimensions,
    Length,
    Codec,
}
impl std::fmt::Display for TextureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid prepared texture: {self:?}")
    }
}
impl std::error::Error for TextureError {}
impl TextureDescriptor {
    /// Validate dimensions and return the exact unwrapped byte extent.
    pub fn byte_len(self) -> Result<u64, TextureError> {
        let l = self.layout;
        if l.logical_size.contains(&0)
            || l.content_size.contains(&0)
            || l.storage_size.contains(&0)
            || (0..2).any(|i| {
                l.content_size[i] > l.storage_size[i]
                    || !l.storage_size[i].is_multiple_of(self.format.block_size())
            })
        {
            return Err(TextureError::Dimensions);
        }
        self.format
            .byte_len(l.storage_size)
            .filter(|&n| n <= isize::MAX as u64)
            .ok_or(TextureError::Length)
    }
    /// Validate actual bytes at the resource boundary without decoding pixels.
    pub fn validate(self, bytes: usize) -> Result<(), TextureError> {
        if self.byte_len()? == bytes as u64 {
            Ok(())
        } else {
            Err(TextureError::Length)
        }
    }
}
/// A checked, fully accounted CPU payload. All colors are linear premultiplied;
/// encoding the original sRGB pixels is the offline tool's responsibility.
#[derive(Debug)]
pub struct TexturePayload {
    descriptor: TextureDescriptor,
    bytes: OwnedBytes,
}
impl TexturePayload {
    /// Validate owned storage once; rejection drops its existing reservation.
    pub fn new(descriptor: TextureDescriptor, bytes: OwnedBytes) -> Result<Self, TextureError> {
        descriptor.validate(bytes.as_slice().len())?;
        Ok(Self { descriptor, bytes })
    }
    /// Transfer validated metadata and its original allocation without copying.
    pub fn into_parts(self) -> (TextureDescriptor, OwnedBytes) {
        (self.descriptor, self.bytes)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// File storage codec; unwrapping BcCrunch produces BC blocks rather than pixels.
pub enum TextureCodec {
    /// The file stores the descriptor's exact packed pixel/block extent.
    Raw,
    /// bc-crunch range coding; only BC1 and BC3 descriptors accept this codec.
    BcCrunch,
}
/// REELTEX1: fixed little-endian metadata followed by exactly payload_len bytes.
/// The container/asset version provides integrity; no native layout is implicit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureHeader {
    descriptor: TextureDescriptor,
    codec: TextureCodec,
    payload_len: u64,
}
impl TextureHeader {
    /// Encoded v1 header extent in bytes.
    pub const LEN: usize = 60;
    /// Identifies the portable prepared-texture file envelope.
    pub const MAGIC: &'static [u8; 8] = b"REELTEX1";
    /// Validate a descriptor and its on-disk payload extent before serialization.
    pub fn new(
        descriptor: TextureDescriptor,
        codec: TextureCodec,
        payload_len: u64,
    ) -> Result<Self, TextureError> {
        let decoded = descriptor.byte_len()?;
        if payload_len == 0
            || payload_len
                .checked_add(Self::LEN as u64)
                .is_none_or(|n| n > isize::MAX as u64)
        {
            return Err(TextureError::Length);
        }
        match codec {
            TextureCodec::Raw if payload_len != decoded => return Err(TextureError::Length),
            TextureCodec::BcCrunch if !descriptor.format.is_compressed() => {
                return Err(TextureError::Codec);
            }
            _ => {}
        }
        Ok(Self {
            descriptor,
            codec,
            payload_len,
        })
    }
    /// Validated decoded storage metadata.
    pub fn descriptor(self) -> TextureDescriptor {
        self.descriptor
    }
    /// Codec used only by the file payload.
    pub fn codec(self) -> TextureCodec {
        self.codec
    }
    /// Exact encoded payload extent, excluding the header.
    pub fn payload_len(self) -> u64 {
        self.payload_len
    }
    /// Serialize fixed little-endian metadata; append the separate payload unchanged.
    pub fn encode(self) -> [u8; Self::LEN] {
        let mut out = [0; Self::LEN];
        out[..8].copy_from_slice(Self::MAGIC);
        let words = [
            self.descriptor.format.code(),
            match self.codec {
                TextureCodec::Raw => 0,
                TextureCodec::BcCrunch => 1,
            },
            match self.descriptor.source_encoding {
                ColorEncoding::Srgb => 0,
                ColorEncoding::Linear => 1,
            },
            self.descriptor.layout.logical_size[0],
            self.descriptor.layout.logical_size[1],
            self.descriptor.layout.content_size[0],
            self.descriptor.layout.content_size[1],
            self.descriptor.layout.storage_size[0],
            self.descriptor.layout.storage_size[1],
        ];
        for (chunk, value) in out[8..44].as_chunks_mut::<4>().0.iter_mut().zip(words) {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        out[44..52].copy_from_slice(
            &self
                .descriptor
                .byte_len()
                .expect("validated descriptor")
                .to_le_bytes(),
        );
        out[52..].copy_from_slice(&self.payload_len.to_le_bytes());
        out
    }
    /// Parse exactly one header, including format, codec and allocation limits.
    pub fn decode(header: &[u8]) -> Result<Self, TextureError> {
        if header.len() != Self::LEN || &header[..8] != Self::MAGIC {
            return Err(TextureError::Header);
        }
        let word = |at| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
        let descriptor = TextureDescriptor {
            format: TextureFormat::from_code(word(8))?,
            source_encoding: match word(16) {
                0 => ColorEncoding::Srgb,
                1 => ColorEncoding::Linear,
                _ => return Err(TextureError::Format),
            },
            layout: TextureLayout {
                logical_size: [word(20), word(24)],
                content_size: [word(28), word(32)],
                storage_size: [word(36), word(40)],
            },
        };
        if descriptor.byte_len()? != u64::from_le_bytes(header[44..52].try_into().unwrap()) {
            return Err(TextureError::Length);
        }
        Self::new(
            descriptor,
            match word(12) {
                0 => TextureCodec::Raw,
                1 => TextureCodec::BcCrunch,
                _ => return Err(TextureError::Codec),
            },
            u64::from_le_bytes(header[52..60].try_into().unwrap()),
        )
    }
    /// Parse the file envelope, validating its exact payload extent before allocation.
    pub fn parse(bytes: &[u8]) -> Result<(Self, &[u8]), TextureError> {
        let header = Self::decode(bytes.get(..Self::LEN).ok_or(TextureError::Header)?)?;
        let payload = &bytes[Self::LEN..];
        if payload.len() as u64 != header.payload_len {
            return Err(TextureError::Length);
        }
        Ok((header, payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prepared_block_layout_and_file_envelope_use_exact_extents() {
        let d = TextureDescriptor {
            format: TextureFormat::Bc3Rgba,
            source_encoding: ColorEncoding::Srgb,
            layout: TextureLayout {
                logical_size: [1920, 1080],
                content_size: [960, 540],
                storage_size: [1024, 1024],
            },
        };
        assert_eq!(d.byte_len(), Ok(1 << 20));
        assert_eq!(d.format.row_bytes(1024), Some(4096));
        assert_eq!(d.layout.uv_scale(), [1. / 2048., 1. / 2048.]);
        let h = TextureHeader::new(d, TextureCodec::Raw, 1 << 20).unwrap();
        assert_eq!(TextureHeader::decode(&h.encode()), Ok(h));
        let mut bad = d;
        bad.layout.storage_size[1] = 1023;
        assert_eq!(bad.byte_len(), Err(TextureError::Dimensions));
        let mut file = h.encode().to_vec();
        assert_eq!(TextureHeader::parse(&file), Err(TextureError::Length));
        file.resize(TextureHeader::LEN + (1 << 20), 0);
        assert_eq!(TextureHeader::parse(&file).unwrap().1.len(), 1 << 20);
    }
}
