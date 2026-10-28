use super::GpuError;
use reel_core::texture::{TextureDescriptor, TextureFormat, TextureLayout};

/// Recently retired storage candidates. A slot may still have external readers;
/// adapters must require released source leases and unique ownership when taking
/// it. Replacing or clearing a slot only releases this pool's own reference.
pub(crate) struct RetiredImages<T> {
    slots: [Option<RetiredImage<T>>; 64],
    next: usize,
}
struct RetiredImage<T> {
    value: T,
    age: u8,
    uses: u8,
}
impl<T> Default for RetiredImages<T> {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            next: 0,
        }
    }
}
impl<T> RetiredImages<T> {
    pub(crate) fn put(&mut self, image: T, uses: u8) {
        self.slots[self.next] = Some(RetiredImage {
            value: image,
            age: 0,
            uses,
        });
        self.next = (self.next + 1) % self.slots.len();
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().flatten().map(|slot| &slot.value)
    }
    pub(crate) fn take_if(&mut self, matches: impl Fn(&T) -> bool) -> Option<T> {
        let index = self
            .slots
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|slot| matches(&slot.value)))?;
        self.slots[index].take().map(|slot| slot.value)
    }
    /// Age only accepted render submissions; polling and failed preparation do
    /// not shorten reuse windows. Repeated storage reuse earns up to 32 frames.
    pub(crate) fn submitted(&mut self) {
        for slot in self.slots.iter_mut().flatten() {
            slot.age = slot.age.saturating_add(1);
        }
    }
    /// A cold pool reference retires only after external readers and the source
    /// lease have released it. Other readers keep their independent ownership.
    pub(crate) fn prune_cold(&mut self, reusable: impl Fn(&T) -> bool) {
        for slot in &mut self.slots {
            if slot.as_ref().is_some_and(|entry| {
                let retention = 2 + entry.uses.saturating_mul(2).min(30);
                entry.age >= retention && reusable(&entry.value)
            }) {
                *slot = None;
            }
        }
    }
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Initialized CPU upload capacity. Native upload APIs finish reading this slice
/// before returning, so later uploads may overwrite it without a GPU wait.
#[cfg(any(feature = "gxm", test))]
#[derive(Default)]
pub(crate) struct UploadScratch {
    bytes: Vec<u8>,
    ticket: Option<reel_core::budget::BudgetTicket>,
}
#[cfg(any(feature = "gxm", test))]
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
    pub(crate) mip: u32,
    pub(crate) offset: u64,
}
impl TextureUploadLayout {
    /// Resolve a flat upload-row cursor without retaining per-level allocations.
    #[cfg(any(feature = "wgpu", test))]
    pub(crate) fn at_row(self, descriptor: TextureDescriptor, mut row: u32) -> (Self, u32) {
        for mip in 0..descriptor.mip_count {
            let size = self.layout.storage_size.map(|v| (v >> mip).max(1));
            let rows = size[1].div_ceil(descriptor.format.block_size());
            if row < rows {
                return (
                    Self {
                        layout: TextureLayout {
                            storage_size: size,
                            ..self.layout
                        },
                        rows,
                        mip,
                        offset: descriptor.mip_offset(mip).expect("validated mip"),
                        row_bytes: descriptor.format.row_bytes(size[0]).unwrap(),
                        source_row_bytes: descriptor
                            .format
                            .row_bytes(descriptor.mip_size(mip)[0])
                            .unwrap(),
                        ..self
                    },
                    row,
                );
            }
            row -= rows;
        }
        unreachable!("upload cursor within validated mip chain")
    }
    pub(crate) fn new(descriptor: TextureDescriptor) -> Result<Self, GpuError> {
        descriptor.byte_len().map_err(|_| GpuError::InvalidSize)?;
        let mut layout = descriptor.layout;
        if descriptor.format == TextureFormat::Rgba8 && descriptor.mip_count == 1 {
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
            rows: (0..descriptor.mip_count)
                .map(|i| {
                    (layout.storage_size[1] >> i)
                        .max(1)
                        .div_ceil(descriptor.format.block_size())
                })
                .sum(),
            mip: 0,
            offset: 0,
            resident_bytes: TextureDescriptor {
                layout,
                ..descriptor
            }
            .byte_len()
            .map_err(|_| GpuError::InvalidSize)?,
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

#[cfg(any(feature = "gxm", test))]
static SRGB_TO_LINEAR_U8: LazyLock<[u8; 256]> =
    LazyLock::new(|| SRGB_TO_LINEAR_F16.map(linear_f16_u8));

#[cfg(any(feature = "gxm", test))]
fn linear_f16_u8(bits: u16) -> u8 {
    (half::f16::from_bits(bits).to_f32().clamp(0., 1.) * 255.).round() as u8
}

/// Convert into the GXM UNORM working texture. Opaque images use a single
/// lookup per channel; translucent pixels retain the shared color conversion.
#[cfg(any(feature = "gxm", test))]
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
    fn retired_storage_is_bounded_recent_and_keeps_external_owners_accounted() {
        use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
        use std::sync::Arc;
        let budget = BudgetPool::new(
            BudgetCategory::GpuResource,
            QueueLimits {
                max_requests: 128,
                max_bytes: 128 * 4096,
            },
        );
        let mut retired = RetiredImages::default();
        let held = Arc::new((0, budget.reserve(4096).unwrap()));
        retired.put(held.clone(), 0);
        assert!(
            retired
                .take_if(|image| Arc::strong_count(image) == 1)
                .is_none()
        );
        for version in 1..=64 {
            retired.put(Arc::new((version, budget.reserve(4096).unwrap())), 0);
        }
        assert_eq!(retired.iter().count(), 64);
        assert_eq!(
            budget.usage().bytes,
            65 * 4096,
            "an evicted pool reference cannot release another reader's storage"
        );
        assert!(retired.iter().all(|image| image.0 > 0));
        let newest = retired.take_if(|image| image.0 == 64).unwrap();
        assert_eq!(newest.1.bytes(), 4096);
        retired.clear();
        assert_eq!(budget.usage().bytes, 8192);
        drop(newest);
        drop(held);
        assert_eq!(budget.usage().bytes, 0);
    }

    #[test]
    fn retired_storage_ages_only_with_submissions_and_waits_for_readers() {
        use std::sync::Arc;
        let unique = |value: &Arc<u8>| Arc::strong_count(value) == 1;
        let mut retired = RetiredImages::default();
        retired.put(Arc::new(1), 0);
        retired.put(Arc::new(2), u8::MAX);
        let flight = Arc::new(3);
        retired.put(flight.clone(), 0);
        for _ in 0..1000 {
            retired.prune_cold(unique);
        }
        assert_eq!(retired.iter().count(), 3, "polling cannot age storage");
        retired.submitted();
        retired.prune_cold(unique);
        assert_eq!(retired.iter().count(), 3);
        retired.submitted();
        retired.prune_cold(unique);
        assert_eq!(retired.iter().map(|v| **v).collect::<Vec<_>>(), [2, 3]);
        for _ in 2..31 {
            retired.submitted();
            retired.prune_cold(unique);
        }
        assert_eq!(
            retired.iter().count(),
            2,
            "hot storage earns 32 submissions"
        );
        retired.submitted();
        retired.prune_cold(unique);
        assert_eq!(retired.iter().map(|v| **v).collect::<Vec<_>>(), [3]);
        drop(flight);
        retired.prune_cold(unique);
        assert_eq!(retired.iter().count(), 0);
    }

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
                storage_layout: reel_core::texture::TextureStorageLayout::Linear,
                mip_count: 1,
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
                storage_layout: reel_core::texture::TextureStorageLayout::Linear,
                mip_count: 1,
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

#[cfg(test)]
mod mip_tests {
    use super::*;
    #[test]
    fn flat_progress_visits_each_level_and_charges_full_chain() {
        let d = TextureDescriptor {
            mip_count: 5,
            storage_layout: Default::default(),
            format: TextureFormat::Bc3Rgba,
            layout: TextureLayout::identity([16, 8]),
            source_encoding: reel_core::texture::ColorEncoding::Linear,
        };
        let upload = TextureUploadLayout::new(d).unwrap();
        assert_eq!(upload.resident_bytes, 208);
        assert_eq!(upload.rows, 6);
        let mut bytes = 0;
        for row in 0..upload.rows {
            let (level, local) = upload.at_row(d, row);
            assert!(local < level.rows);
            bytes += level.row_bytes;
        }
        assert_eq!(bytes, 208);
    }
}
