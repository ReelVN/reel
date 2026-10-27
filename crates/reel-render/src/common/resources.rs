use super::GpuError;
use reel_core::texture::{TextureDescriptor, TextureFormat, TextureLayout};

/// Initialized CPU upload capacity. Native upload APIs finish reading this slice
/// before returning, so later uploads may overwrite it without a GPU wait.
#[derive(Default)]
pub(crate) struct UploadScratch {
    bytes: Vec<u8>,
    ticket: Option<reel_core::budget::BudgetTicket>,
}
impl UploadScratch {
    pub(crate) fn capacity(&self) -> u64 {
        self.bytes.len() as u64
    }
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
    pub(crate) fn get(
        &mut self,
        pool: &reel_core::budget::BudgetPool,
        limit: u64,
        bytes: usize,
    ) -> Result<&mut [u8], GpuError> {
        if self.bytes.len() < bytes {
            self.clear();
            if bytes as u64 > limit {
                return Err(GpuError::Budget {
                    domain: "image upload scratch",
                    requested: bytes as u64,
                    available: limit,
                });
            }
            let ticket = pool.reserve(bytes as u64).map_err(|_| GpuError::Pressure {
                domain: "image upload scratch",
                requested: bytes as u64,
                available: limit.saturating_sub(pool.usage().bytes),
            })?;
            self.bytes = vec![0; bytes];
            self.ticket = Some(ticket);
        }
        Ok(&mut self.bytes[..bytes])
    }
}

/// Upload a compact prefix of each immutable payload row. Both power-of-two
/// storage and 32-aligned tiled storage have the same packed allocation extent
/// on the supported backends; no new edge pixels are synthesized here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TextureUploadLayout {
    pub(crate) layout: TextureLayout,
    pub(crate) source_row_bytes: u64,
    pub(crate) row_bytes: u64,
    pub(crate) rows: u32,
    pub(crate) resident_bytes: u64,
}
impl TextureUploadLayout {
    pub(crate) fn new(descriptor: TextureDescriptor) -> Result<Self, GpuError> {
        descriptor.byte_len().map_err(|_| GpuError::InvalidSize)?;
        let mut layout = descriptor.layout;
        if descriptor.format == TextureFormat::Rgba8 {
            let content = layout.content_size;
            let candidate = |round: fn(u32) -> Option<u32>| {
                round(content[0])
                    .zip(round(content[1]))
                    .map(|(w, h)| [w, h])
            };
            let pixels = |size: [u32; 2]| u64::from(size[0]) * u64::from(size[1]);
            for size in [
                candidate(u32::checked_next_power_of_two),
                candidate(|n| n.checked_add(31).map(|n| n & !31)),
            ]
            .into_iter()
            .flatten()
            {
                if size
                    .iter()
                    .zip(descriptor.layout.storage_size)
                    .all(|(&n, source)| n <= source)
                    && pixels(size) < pixels(layout.storage_size)
                {
                    layout.storage_size = size;
                }
            }
        }
        Ok(Self {
            layout,
            source_row_bytes: descriptor
                .format
                .row_bytes(descriptor.layout.storage_size[0])
                .ok_or(GpuError::InvalidSize)?,
            row_bytes: descriptor
                .format
                .row_bytes(layout.storage_size[0])
                .ok_or(GpuError::InvalidSize)?,
            rows: layout.storage_size[1] / descriptor.format.block_size(),
            resident_bytes: descriptor
                .format
                .byte_len(layout.storage_size)
                .ok_or(GpuError::InvalidSize)?,
        })
    }
}
/// Cached device storage borrows resource identity without owning its CPU payload.
/// Retirement closes new leases; existing frames remain valid until their final
/// lease is released, including while an upload is still progressing.
pub(crate) struct ResourceSource {
    pub(crate) version: reel_ui::resources::ResourceVersion,
    pub(crate) lease: reel_ui::resources::ResourceWeak,
}
impl ResourceSource {
    pub(crate) fn new(lease: &reel_ui::resources::ResourceLease) -> Self {
        Self {
            version: lease.version().clone(),
            lease: lease.downgrade(),
        }
    }
    pub(crate) fn retained(&self, evicted: &[reel_ui::resources::ResourceVersion]) -> bool {
        !self.lease.is_released() && !evicted.contains(&self.version)
    }
}
pub fn upload_cost(size: [u32; 2]) -> Result<u64, GpuError> {
    // Convert directly into mapped staging. Only staging and GPU residency coexist.
    let row = u64::from(size[0])
        .checked_mul(8)
        .ok_or(GpuError::InvalidSize)?;
    let aligned = row.div_ceil(256) * 256;
    (aligned + row)
        .checked_mul(u64::from(size[1]))
        .ok_or(GpuError::InvalidSize)
}

use reel_ui::{
    peniko::{
        ImageAlphaType,
        color::{AlphaColor, LinearSrgb, PremulColor, Srgb},
    },
    resources::ColorEncoding,
};
use std::sync::LazyLock;
static SRGB_TO_LINEAR: LazyLock<[f32; 256]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        AlphaColor::<Srgb>::new([i as f32 / 255., 0., 0., 1.])
            .convert::<LinearSrgb>()
            .components[0]
    })
});

static SRGB_TO_LINEAR_F16: LazyLock<[u16; 256]> =
    LazyLock::new(|| std::array::from_fn(|i| half::f16::from_f32(SRGB_TO_LINEAR[i]).to_bits()));
#[cfg(any(feature = "wgpu", test))]
static NORMALIZED_F16: LazyLock<[u16; 256]> =
    LazyLock::new(|| std::array::from_fn(|i| half::f16::from_f32(i as f32 / 255.).to_bits()));

#[cfg(any(feature = "gles2", test))]
static SRGB_TO_LINEAR_U8: LazyLock<[u8; 256]> =
    LazyLock::new(|| SRGB_TO_LINEAR_F16.map(linear_f16_u8));

#[cfg(any(feature = "gles2", test))]
fn linear_f16_u8(bits: u16) -> u8 {
    (half::f16::from_bits(bits).to_f32().clamp(0., 1.) * 255.).round() as u8
}

/// Convert into the GLES UNORM working texture. Opaque images use a single
/// lookup per channel; translucent pixels retain the shared color conversion.
#[cfg(any(feature = "gles2", test))]
pub(crate) fn rgba8_linear_u8(
    rgba: &[u8],
    encoding: ColorEncoding,
    alpha_type: ImageAlphaType,
) -> [u8; 4] {
    if rgba[3] == 0 {
        return [0; 4];
    }
    if rgba[3] == 255 {
        return match encoding {
            ColorEncoding::Srgb => {
                let table = &*SRGB_TO_LINEAR_U8;
                [
                    table[rgba[0] as usize],
                    table[rgba[1] as usize],
                    table[rgba[2] as usize],
                    255,
                ]
            }
            ColorEncoding::Linear => [rgba[0], rgba[1], rgba[2], 255],
        };
    }
    translucent_linear_f16(rgba, encoding, alpha_type).map(linear_f16_u8)
}

#[cfg(any(feature = "wgpu", test))]
pub(crate) fn rgba8_linear_f16(
    rgba: &[u8],
    encoding: ColorEncoding,
    alpha_type: ImageAlphaType,
) -> [u16; 4] {
    if rgba[3] == 0 {
        // Hidden RGB must not become visible in interpolation or masks.
        return [0; 4];
    }
    if rgba[3] == 255 {
        // Opaque channels have only 256 possible results. Reuse their exact
        // half encodings rather than converting every channel of every pixel.
        let table = match encoding {
            ColorEncoding::Srgb => &*SRGB_TO_LINEAR_F16,
            ColorEncoding::Linear => &*NORMALIZED_F16,
        };
        return [
            table[rgba[0] as usize],
            table[rgba[1] as usize],
            table[rgba[2] as usize],
            0x3c00,
        ];
    }
    translucent_linear_f16(rgba, encoding, alpha_type)
}

fn translucent_linear_f16(
    rgba: &[u8],
    encoding: ColorEncoding,
    alpha_type: ImageAlphaType,
) -> [u16; 4] {
    let components = std::array::from_fn(|i| f32::from(rgba[i]) / 255.);
    let linear = match (encoding, alpha_type) {
        (ColorEncoding::Srgb, ImageAlphaType::Alpha) => {
            let alpha = components[3];
            [
                SRGB_TO_LINEAR[rgba[0] as usize] * alpha,
                SRGB_TO_LINEAR[rgba[1] as usize] * alpha,
                SRGB_TO_LINEAR[rgba[2] as usize] * alpha,
                alpha,
            ]
        }
        (ColorEncoding::Srgb, ImageAlphaType::AlphaPremultiplied) => {
            PremulColor::<Srgb>::new(components)
                .convert::<LinearSrgb>()
                .components
        }
        (ColorEncoding::Linear, ImageAlphaType::Alpha) => {
            AlphaColor::<LinearSrgb>::new(components)
                .premultiply()
                .components
        }
        (ColorEncoding::Linear, ImageAlphaType::AlphaPremultiplied) => components,
    };
    linear.map(|value| half::f16::from_f32(value).to_bits())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_scratch_retains_initialized_capacity_and_returns_its_budget_under_pressure() {
        use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
        let pool = BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: 4,
                max_bytes: 4096,
            },
        );
        let mut scratch = UploadScratch::default();
        let first = scratch.get(&pool, 4096, 4096).unwrap();
        let address = first.as_ptr();
        first.fill(123);
        for _ in 0..32 {
            let next = scratch.get(&pool, 4096, 2048).unwrap();
            assert_eq!(next.as_ptr(), address);
            assert!(
                next.iter().all(|byte| *byte == 123),
                "reused initialized capacity is not zeroed again"
            );
        }
        assert_eq!(pool.usage().bytes, 4096);
        assert!(scratch.get(&pool, 4096, 8192).is_err());
        assert_eq!(pool.usage().bytes, 0);
        assert_eq!(scratch.capacity(), 0);
        eprintln!(
            "32 repeated upload slices: allocations=1 zero_initialized_bytes=4096 (former allocations=33 zero_initialized_bytes=69632)"
        );
    }

    #[test]
    fn texture_upload_compacts_padding_without_expanding_small_or_block_storage() {
        for (content, storage, expected) in [
            ([960, 544], [1024, 1024], [960, 544]),
            ([8, 8], [8, 8], [8, 8]),
            ([12, 270], [16, 512], [16, 512]),
            ([12, 270], [32, 512], [16, 512]),
            ([20, 270], [32, 512], [32, 288]),
            ([8, 100], [8, 2048], [8, 128]),
            ([32, 8], [64, 16], [32, 8]),
            ([31, 17], [31, 17], [31, 17]),
            ([4095, 1080], [4096, 4096], [4096, 1088]),
        ] {
            let descriptor = TextureDescriptor {
                format: TextureFormat::Rgba8,
                source_encoding: ColorEncoding::Linear,
                layout: TextureLayout {
                    logical_size: [1920, 1080],
                    content_size: content,
                    storage_size: storage,
                },
            };
            let upload = TextureUploadLayout::new(descriptor).unwrap();
            assert_eq!(upload.layout.storage_size, expected);
            assert_eq!(upload.layout.content_size, content);
            assert_eq!(upload.layout.logical_size, descriptor.layout.logical_size);
            assert_eq!(upload.source_row_bytes, u64::from(storage[0]) * 4);
            assert_eq!(upload.row_bytes, u64::from(expected[0]) * 4);
            assert_eq!(upload.rows, expected[1]);
            assert_eq!(
                upload.resident_bytes,
                u64::from(expected[0]) * u64::from(expected[1]) * 4
            );
            for axis in 0..2 {
                let source = descriptor.layout.uv_scale()[axis] * f64::from(storage[axis]);
                let target = upload.layout.uv_scale()[axis] * f64::from(expected[axis]);
                assert!(
                    (source - target).abs() < 1e-12,
                    "authored coordinates retain the same content texel"
                );
            }
        }
        for format in [
            TextureFormat::Bc1Rgb,
            TextureFormat::Bc3Rgba,
            TextureFormat::LinearRgba16Float,
            TextureFormat::Bgra8,
        ] {
            let descriptor = TextureDescriptor {
                format,
                source_encoding: ColorEncoding::Linear,
                layout: TextureLayout {
                    logical_size: [1920, 1080],
                    content_size: [960, 544],
                    storage_size: [1024, 1024],
                },
            };
            let upload = TextureUploadLayout::new(descriptor).unwrap();
            assert_eq!(upload.layout, descriptor.layout);
            assert_eq!(upload.resident_bytes, descriptor.byte_len().unwrap());
            assert_eq!(upload.source_row_bytes, upload.row_bytes);
        }
    }

    #[test]
    fn unorm_upload_preserves_color_and_hidden_alpha() {
        for encoding in [ColorEncoding::Srgb, ColorEncoding::Linear] {
            for alpha_type in [ImageAlphaType::Alpha, ImageAlphaType::AlphaPremultiplied] {
                for alpha in 0..=255u8 {
                    for value in 0..=255u8 {
                        let mut rgba = [value, 255 - value, value.rotate_left(3), alpha];
                        if alpha_type == ImageAlphaType::AlphaPremultiplied {
                            for channel in &mut rgba[..3] {
                                *channel = (u16::from(*channel) * u16::from(alpha) / 255) as u8;
                            }
                        }
                        assert_eq!(
                            rgba8_linear_u8(&rgba, encoding, alpha_type),
                            rgba8_linear_f16(&rgba, encoding, alpha_type).map(linear_f16_u8),
                            "encoding={encoding:?} alpha={alpha_type:?} rgba={rgba:?}",
                        );
                    }
                }
            }
        }
    }
}
