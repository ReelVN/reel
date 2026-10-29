//! Portable, linear-premultiplied texture storage and its offline file envelope.
//! BC payloads carry an explicit block layout selected by offline target preparation.
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
/// Physical ordering of the payload's storage blocks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TextureStorageLayout {
    /// Standard row-major pixels or BC blocks.
    #[default]
    Linear,
    /// GXM BC blocks with y in the low Morton bit; rectangular axes interleave
    /// until the shorter axis is exhausted, then append the longer axis bits.
    GxmBcMorton,
}
impl TextureStorageLayout {
    /// Byte offset of a validated base-level compressed block.
    pub fn block_offset(self, size: [u32; 2], block_bytes: usize, x: u32, y: u32) -> usize {
        let [width, height] = size.map(|n| n.div_ceil(4));
        let index = match self {
            Self::Linear => y as usize * width as usize + x as usize,
            Self::GxmBcMorton => {
                let (mut address, mut bit) = (0usize, 0);
                for axis in 0..width.ilog2().max(height.ilog2()) {
                    if axis < height.ilog2() {
                        address |= (((y >> axis) & 1) as usize) << bit;
                        bit += 1;
                    }
                    if axis < width.ilog2() {
                        address |= (((x >> axis) & 1) as usize) << bit;
                        bit += 1;
                    }
                }
                address
            }
        };
        index * block_bytes
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
/// One logical image inside a physical texture, expressed in texels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImageView {
    /// Author-space dimensions used for layout, cropping and slices.
    pub logical_size: [u32; 2],
    /// Top-left content texel in the physical texture.
    pub content_origin: [u32; 2],
    /// Visible content texel extent, excluding gutter and page padding.
    pub content_size: [u32; 2],
}
impl ImageView {
    /// Construct the ordinary origin-zero view of a prepared texture.
    pub const fn from_layout(layout: TextureLayout) -> Self {
        Self {
            logical_size: layout.logical_size,
            content_origin: [0; 2],
            content_size: layout.content_size,
        }
    }
    /// Check that the view fits its physical storage and has positive extents.
    pub fn validate(self, storage: [u32; 2]) -> Result<(), TextureError> {
        if self.logical_size.contains(&0)
            || self.content_size.contains(&0)
            || (0..2).any(|axis| {
                self.content_origin[axis]
                    .checked_add(self.content_size[axis])
                    .is_none_or(|end| end > storage[axis])
            })
        {
            return Err(TextureError::Dimensions);
        }
        Ok(())
    }
    /// Convert author coordinates to normalized physical texture coordinates.
    pub fn uv_transform(self, storage: [u32; 2]) -> ([f64; 2], [f64; 2]) {
        (
            std::array::from_fn(|i| {
                f64::from(self.content_size[i])
                    / f64::from(self.logical_size[i])
                    / f64::from(storage[i])
            }),
            std::array::from_fn(|i| f64::from(self.content_origin[i]) / f64::from(storage[i])),
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Format, coordinate mapping and source transfer metadata for prepared storage.
pub struct TextureDescriptor {
    /// Explicit storage order, shared by each mip level.
    pub storage_layout: TextureStorageLayout,
    /// Number of packed mip levels, beginning at the base storage extent.
    pub mip_count: u32,
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
        if self.mip_count == 0
            || self.mip_count
                > 32 - l
                    .storage_size
                    .into_iter()
                    .max()
                    .unwrap_or(0)
                    .leading_zeros()
        {
            return Err(TextureError::Dimensions);
        }
        if self.storage_layout == TextureStorageLayout::GxmBcMorton
            && (!self.format.is_compressed()
                || l.storage_size
                    .iter()
                    .any(|&n| n < 4 || !n.is_power_of_two()))
        {
            return Err(TextureError::Dimensions);
        }
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
        (0..self.mip_count)
            .try_fold(0u64, |total, level| {
                total
                    .checked_add(self.format.byte_len(self.mip_size(level))?)
                    .filter(|&n| n <= isize::MAX as u64)
            })
            .ok_or(TextureError::Length)
    }
    /// Stored texel extent at a level; each axis halves with a minimum of one.
    pub fn mip_size(self, level: u32) -> [u32; 2] {
        self.layout
            .storage_size
            .map(|axis| axis.checked_shr(level).unwrap_or(0).max(1))
    }
    /// Packed byte offset of a level in the validated contiguous payload.
    pub fn mip_offset(self, level: u32) -> Option<u64> {
        if level >= self.mip_count {
            return None;
        }
        (0..level).try_fold(0u64, |sum, i| {
            sum.checked_add(self.format.byte_len(self.mip_size(i))?)
        })
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
    /// bc-crunch range coding; accepts one linear BC1/BC3 level.
    /// Mip chains use Raw storage and the enclosing archive compression.
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
    pub const LEN: usize = 68;
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
            TextureCodec::BcCrunch
                if !descriptor.format.is_compressed()
                    || descriptor.mip_count != 1
                    || descriptor.storage_layout != TextureStorageLayout::Linear =>
            {
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
        out[52..60].copy_from_slice(&self.payload_len.to_le_bytes());
        let layout: u32 = match self.descriptor.storage_layout {
            TextureStorageLayout::Linear => 0,
            TextureStorageLayout::GxmBcMorton => 1,
        };
        out[60..64].copy_from_slice(&layout.to_le_bytes());
        out[64..68].copy_from_slice(&self.descriptor.mip_count.to_le_bytes());
        out
    }
    /// Parse exactly one header, including format, codec and allocation limits.
    pub fn decode(header: &[u8]) -> Result<Self, TextureError> {
        if header.len() != Self::LEN || &header[..8] != Self::MAGIC {
            return Err(TextureError::Header);
        }
        let word = |at| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
        let descriptor = TextureDescriptor {
            storage_layout: match word(60) {
                0 => TextureStorageLayout::Linear,
                1 => TextureStorageLayout::GxmBcMorton,
                _ => return Err(TextureError::Format),
            },
            mip_count: word(64),
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
    fn morton_order_is_explicit_validated_and_rectangular() {
        let layout = TextureStorageLayout::GxmBcMorton;
        assert_eq!(
            (0..4)
                .map(|x| layout.block_offset([16, 8], 8, x, 0))
                .collect::<Vec<_>>(),
            [0, 16, 32, 48]
        );
        assert_eq!(
            (0..4)
                .map(|x| layout.block_offset([16, 8], 8, x, 1))
                .collect::<Vec<_>>(),
            [8, 24, 40, 56]
        );
        let mut d = TextureDescriptor {
            format: TextureFormat::Bc1Rgb,
            storage_layout: layout,
            mip_count: 1,
            source_encoding: ColorEncoding::Linear,
            layout: TextureLayout::identity([16, 8]),
        };
        let h = TextureHeader::new(d, TextureCodec::Raw, 64).unwrap();
        assert_eq!(TextureHeader::decode(&h.encode()), Ok(h));
        assert_eq!(
            TextureHeader::new(d, TextureCodec::BcCrunch, 10),
            Err(TextureError::Codec)
        );
        d.layout.storage_size = [12, 8];
        assert_eq!(d.byte_len(), Err(TextureError::Dimensions));
        let mut bytes = h.encode();
        bytes[60] = 2;
        assert_eq!(TextureHeader::decode(&bytes), Err(TextureError::Format));
    }

    #[test]
    #[ignore = "CPU measurement; run in release mode"]
    fn morton_upload_measurement() {
        let size = [1024, 1024];
        let bytes = 1024 * 1024;
        let source = vec![19u8; bytes];
        let mut dest = vec![0u8; bytes];
        let start = std::time::Instant::now();
        for _ in 0..100 {
            for y in 0..256 {
                for x in 0..256 {
                    let from = (y as usize * 256 + x as usize) * 16;
                    let to = TextureStorageLayout::GxmBcMorton.block_offset(size, 16, x, y);
                    dest[to..to + 16].copy_from_slice(&source[from..from + 16]);
                }
            }
            std::hint::black_box(&dest);
        }
        let reorder = start.elapsed();
        let start = std::time::Instant::now();
        for _ in 0..100 {
            dest.copy_from_slice(std::hint::black_box(&source));
            std::hint::black_box(&dest);
        }
        eprintln!(
            "100 x 1MiB CPU upload: runtime Morton {:?}, offline direct copy {:?}; source+destination 2MiB in both",
            reorder,
            start.elapsed()
        );
        assert_eq!(source, dest);
    }

    #[test]
    fn prepared_block_layout_and_file_envelope_use_exact_extents() {
        let d = TextureDescriptor {
            storage_layout: crate::texture::TextureStorageLayout::Linear,
            mip_count: 1,
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

#[cfg(test)]
mod mip_tests {
    use super::*;
    #[test]
    fn packed_chain_offsets_and_envelope_validate_every_level() {
        for (size, format, count, expected) in [
            ([7, 3], TextureFormat::Rgba8, 3, 100),
            ([16, 8], TextureFormat::Bc3Rgba, 5, 208),
            ([8, 16], TextureFormat::Bc1Rgb, 5, 104),
        ] {
            let d = TextureDescriptor {
                storage_layout: Default::default(),
                mip_count: count,
                format,
                layout: TextureLayout::identity(size),
                source_encoding: ColorEncoding::Linear,
            };
            assert_eq!(d.byte_len().unwrap(), expected);
            let h = TextureHeader::new(d, TextureCodec::Raw, expected).unwrap();
            assert_eq!(TextureHeader::decode(&h.encode()).unwrap().descriptor(), d);
            let mut offset = 0;
            for level in 0..count {
                assert_eq!(d.mip_offset(level), Some(offset));
                offset += format.byte_len(d.mip_size(level)).unwrap();
            }
            assert_eq!(offset, expected);
            assert!(d.validate(expected as usize - 1).is_err());
            assert!(
                TextureDescriptor {
                    mip_count: count + 1,
                    ..d
                }
                .byte_len()
                .is_err()
            );
        }
    }
}
