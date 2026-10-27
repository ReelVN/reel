pub(super) use super::programs::Target;
use super::*;
use crate::common::resources::ResourceSource;
use reel_core::texture::{TextureFormat, TextureLayout};
use reel_ui::{
    peniko::{ImageAlphaType, ImageFormat},
    resources::ResourceData,
};

pub(super) struct Image {
    pub texture: native::Texture,
    pub planes: Vec<native::Texture>,
    pub video: [[f32; 4]; 4],
    pub kind: f32,
    pub layout: TextureLayout,
    pub format: Option<TextureFormat>,
    pub encoding: ColorEncoding,
    pub coverage: bool,
    pub source: Option<ResourceSource>,
    _storage: BudgetTicket,
}
impl Image {
    pub fn uv_transform(&self) -> reel_ui::kurbo::Affine {
        let [x, y] = self.layout.uv_scale();
        reel_ui::kurbo::Affine::scale_non_uniform(x, y)
    }
}

struct Pending {
    source: ResourceLease,
    image: Rc<Image>,
    plane: usize,
    row: u32,
}
#[derive(Default)]
pub(super) struct Images {
    pub entries: HashMap<ResourceHandle, Rc<Image>>,
    pending: HashMap<ResourceHandle, Pending>,
    white: Option<Rc<Image>>,
    failures: Vec<BudgetTicket>,
    scratch: crate::common::resources::UploadScratch,
    retired: crate::common::resources::RetiredImages<Rc<Image>>,
}
impl Images {
    pub fn retain_demand(&mut self, required: &[crate::common::image_plan::Required<'_>]) {
        self.entries.retain(|handle, image| {
            Rc::strong_count(image) > 1
                || required.iter().any(|item| item.source.handle() == *handle)
        });
        self.retired.clear();
    }
    pub fn release_idle(&mut self) {
        self.scratch.clear();
        self.retired.clear();
    }
    /// Validate the simultaneous image demand before issuing an upload command.
    pub fn validate(
        &self,
        required: &[crate::common::image_plan::Required<'_>],
        caps: &native::Capabilities,
        limits: crate::common::GpuLimits,
        total_bytes: u64,
    ) -> Result<u64, GlesError> {
        let mut resident = 0u64;
        let mut additional = 0u64;
        let mut staging = 0;
        let mut pending_staging = 0;
        let mut claimed = [false; 64];
        for item in required {
            let (bytes, upload) = match item.source.data() {
                ResourceData::Image { image, .. } => (
                    u64::from(image.width) * u64::from(image.height) * 4,
                    u64::from(image.width) * 4,
                ),
                ResourceData::Texture { descriptor, .. } => {
                    if match descriptor.format {
                        TextureFormat::Bc1Rgb | TextureFormat::Bc3Rgba => !caps.bc,
                        TextureFormat::Bgra8 => !caps.bgra,
                        TextureFormat::LinearRgba16Float => !caps.half_float,
                        TextureFormat::Rgba8 => false,
                    } {
                        return Err(GlesError::Unsupported {
                            at: item.at,
                            record: "device texture format",
                        });
                    }
                    if descriptor.format.is_compressed()
                        && descriptor
                            .layout
                            .storage_size
                            .iter()
                            .any(|&n| n < 8 || !n.is_power_of_two())
                    {
                        return Err(GlesError::Unsupported {
                            at: item.at,
                            record: "native BC texture dimensions",
                        });
                    }
                    let upload = item.texture.expect("planned texture");
                    let bytes = upload.resident_bytes;
                    (
                        bytes,
                        if descriptor.format.is_compressed() {
                            bytes
                        } else if upload.source_row_bytes != upload.row_bytes {
                            upload.row_bytes
                        } else {
                            0
                        },
                    )
                }
                ResourceData::Yuv420(image) => (
                    (0..image.plane_count())
                        .map(|i| u64::from(image.row_bytes(i)) * u64::from(image.plane_size(i)[1]))
                        .sum(),
                    (0..image.plane_count())
                        .filter(|&i| image.row_stride(i) != image.row_bytes(i))
                        .map(|i| u64::from(image.row_bytes(i)))
                        .max()
                        .unwrap_or(0),
                ),
                _ => unreachable!("common image demand validates resource kinds"),
            };
            resident = resident.checked_add(bytes).ok_or(GlesError::InvalidSize)?;
            if !self.entries.contains_key(&item.source.handle()) {
                pending_staging = pending_staging.max(upload);
                if !self.pending.contains_key(&item.source.handle()) {
                    let reusable = self
                        .retired
                        .iter()
                        .enumerate()
                        .find(|(index, image)| !claimed[*index] && recyclable(image, item));
                    if let Some((index, _)) = reusable {
                        claimed[index] = true;
                    } else {
                        additional = additional
                            .checked_add(bytes)
                            .ok_or(GlesError::InvalidSize)?;
                    }
                }
            }
            staging = staging.max(upload);
        }
        if staging > limits.transient_bytes {
            return Err(GlesError::Budget {
                domain: "image upload staging",
                requested: staging,
                available: limits.transient_bytes,
            });
        }
        crate::common::image_plan::peak(std::iter::once((resident, 0)), limits.resource_bytes)?;
        crate::common::image_plan::peak(std::iter::once((resident, staging)), total_bytes)?;
        additional
            .checked_add(pending_staging)
            .ok_or(GlesError::InvalidSize)
    }
    pub fn prune(&mut self, versions: &[ResourceVersion]) {
        for (_, image) in self.entries.extract_if(|_, image| {
            !image
                .source
                .as_ref()
                .is_some_and(|source| source.retained(versions))
        }) {
            if image.kind == 0. && !image.coverage {
                self.retired.put(image);
            }
        }
        self.pending.retain(|_, pending| {
            // The pending upload itself owns a valid pre-retirement lease.
            // prepare() discards it when the next demand no longer includes it.
            !versions.contains(pending.source.version())
        });
    }
    fn make_room(&mut self, budget: &BudgetPool, cost: u64, limit: u64) {
        if cost > limit.saturating_sub(budget.usage().bytes) {
            self.retired.clear();
            self.entries.retain(|_, image| Rc::strong_count(image) > 1);
        }
    }
    pub fn after_drain(&mut self) {
        self.failures.clear();
    }
    pub fn white(
        &mut self,
        gpu: &native::Context,
        budgets: &Budgets,
        limit: u64,
    ) -> Result<Rc<Image>, GlesError> {
        if let Some(image) = &self.white {
            return Ok(image.clone());
        }
        let storage = reserve(&budgets.resources, 4, limit, "white texture")?;
        let built = (|| {
            let texture = gpu.create_texture([1, 1], native::TextureFormat::Rgba8)?;
            gpu.upload(&texture, [0, 0], [1, 1], &[255; 4], 4)?;
            Ok::<_, GlesError>(texture)
        })();
        let texture = match built {
            Ok(value) => value,
            Err(error) => {
                if gpu.finish().is_err() {
                    self.failures.push(storage);
                }
                return Err(error);
            }
        };
        let image = Rc::new(Image {
            texture,
            planes: Vec::new(),
            video: [[0.; 4]; 4],
            kind: 0.,
            layout: TextureLayout::identity([1, 1]),
            format: None,
            encoding: ColorEncoding::Linear,
            coverage: false,
            source: None,
            _storage: storage,
        });
        self.white = Some(image.clone());
        Ok(image)
    }
    fn allocate(
        &mut self,
        gpu: &native::Context,
        budgets: &Budgets,
        limits: crate::common::GpuLimits,
        item: &crate::common::image_plan::Required<'_>,
    ) -> Result<Pending, GlesError> {
        let source = item.source;
        let at = item.at;
        let (layout, format, encoding, kind, bytes) = match source.data() {
            ResourceData::Image { image, encoding } => {
                if image.format != ImageFormat::Rgba8 {
                    return Err(GlesError::Unsupported {
                        at,
                        record: "image format",
                    });
                }
                (
                    TextureLayout::identity([image.width, image.height]),
                    None,
                    *encoding,
                    0.,
                    u64::from(image.width) * u64::from(image.height) * 4,
                )
            }
            ResourceData::Texture { descriptor, .. } => {
                let upload = item.texture.expect("planned texture");
                (
                    upload.layout,
                    Some(descriptor.format),
                    descriptor.source_encoding,
                    0.,
                    upload.resident_bytes,
                )
            }
            ResourceData::Yuv420(image) => (
                TextureLayout::identity(image.size()),
                None,
                ColorEncoding::Linear,
                if image.layout() == reel_core::image::Yuv420Layout::Nv12 {
                    2.
                } else {
                    1.
                },
                (0..image.plane_count())
                    .map(|i| u64::from(image.row_bytes(i)) * u64::from(image.plane_size(i)[1]))
                    .sum(),
            ),
            _ => {
                return Err(GlesError::Unsupported {
                    at,
                    record: "non-image upload",
                });
            }
        };
        let recycled = self
            .retired
            .take_if(|image| recyclable(image, item))
            .and_then(Rc::into_inner);
        let (storage, recycled_texture) = if let Some(image) = recycled {
            (image._storage, Some(image.texture))
        } else {
            self.make_room(&budgets.resources, bytes, limits.resource_bytes);
            (
                reserve(
                    &budgets.resources,
                    bytes,
                    limits.resource_bytes,
                    "image residency",
                )?,
                None,
            )
        };
        let built = (|| {
            let mut planes = Vec::new();
            let mut video = [[0.; 4]; 4];
            let texture = if let ResourceData::Yuv420(image) = source.data() {
                for index in 0..image.plane_count() {
                    let format =
                        if index == 1 && image.layout() == reel_core::image::Yuv420Layout::Nv12 {
                            native::TextureFormat::LumaAlpha8
                        } else {
                            native::TextureFormat::Luma8
                        };
                    planes.push(gpu.create_texture(image.plane_size(index), format)?);
                }
                video[..3].copy_from_slice(&image.rgb_rows());
                video[3][0] =
                    f32::from(image.color().transfer == reel_core::image::YuvTransfer::Srgb);
                planes.remove(0)
            } else if format.is_some_and(TextureFormat::is_compressed) {
                // Compressed allocation is inseparable from its complete mip upload.
                let ResourceData::Texture { bytes: blocks, .. } = source.data() else {
                    unreachable!()
                };
                let staging = self.scratch.get(
                    &budgets.transient,
                    limits.transient_bytes,
                    usize::try_from(bytes).map_err(|_| GlesError::InvalidSize)?,
                )?;
                if let Some(texture) = recycled_texture {
                    gpu.upload_bc_texture(&texture, blocks.data(), staging)?;
                    texture
                } else {
                    gpu.create_bc_texture(
                        layout.storage_size,
                        if format == Some(TextureFormat::Bc1Rgb) {
                            native::TextureFormat::Bc1Rgb
                        } else {
                            native::TextureFormat::Bc3Rgba
                        },
                        blocks.data(),
                        staging,
                    )?
                }
            } else {
                if let Some(texture) = recycled_texture {
                    texture
                } else {
                    gpu.create_texture(
                        layout.storage_size,
                        match format {
                            Some(TextureFormat::Bgra8) => native::TextureFormat::Bgra8,
                            Some(TextureFormat::LinearRgba16Float) => {
                                native::TextureFormat::Rgba16Float
                            }
                            _ => native::TextureFormat::Rgba8,
                        },
                    )?
                }
            };
            Ok::<_, GlesError>((texture, planes, video))
        })();
        let (texture, planes, video) = match built {
            Ok(value) => value,
            Err(error) => {
                if gpu.finish().is_err() {
                    self.failures.push(storage);
                }
                return Err(error);
            }
        };
        let image = Rc::new(Image {
            texture,
            planes,
            video,
            kind,
            layout,
            format,
            encoding,
            coverage: false,
            source: Some(ResourceSource {
                version: source.version().clone(),
                lease: source.downgrade(),
            }),
            _storage: storage,
        });
        Ok(Pending {
            source: source.clone(),
            image,
            plane: 0,
            row: 0,
        })
    }
    /// Progress at most the remaining slice, except one complete row or compressed
    /// mip, whose native command is indivisible. Return every touched image so the
    /// submission owner can retain it until the upload fence signals.
    pub fn prepare(
        &mut self,
        device: DeviceResources<'_>,
        required: &[crate::common::image_plan::Required<'_>],
        quota: &mut u64,
        output: &mut HashMap<ResourceHandle, Rc<Image>>,
        touched: &mut Vec<Rc<Image>>,
    ) -> Result<bool, GlesError> {
        let DeviceResources {
            gpu,
            budgets,
            limits,
        } = device;
        let mut ready = true;
        self.pending
            .retain(|handle, _| required.iter().any(|item| item.source.handle() == *handle));
        // Pin all demanded cached textures before an allocation evicts idle entries.
        for item in required {
            let source = item.source;
            if let Some(image) = self.entries.get(&source.handle()) {
                output.insert(source.handle(), image.clone());
            }
        }
        for item in required {
            let source = item.source;
            if output.contains_key(&source.handle()) {
                continue;
            }
            if *quota == 0 {
                ready = false;
                continue;
            }
            if !self.pending.contains_key(&source.handle()) {
                let pending = self.allocate(gpu, budgets, limits, item)?;
                if pending
                    .image
                    .format
                    .is_some_and(TextureFormat::is_compressed)
                {
                    *quota = quota.saturating_sub(pending.image._storage.bytes());
                    touched.push(pending.image.clone());
                    self.entries.insert(source.handle(), pending.image.clone());
                    output.insert(source.handle(), pending.image);
                    continue;
                }
                self.pending.insert(source.handle(), pending);
            }
            let pending = self.pending.get_mut(&source.handle()).unwrap();
            touched.push(pending.image.clone());
            let complete = progress(gpu, budgets, limits, pending, quota, &mut self.scratch)?;
            if complete {
                let pending = self.pending.remove(&source.handle()).unwrap();
                self.entries.insert(source.handle(), pending.image.clone());
                output.insert(source.handle(), pending.image);
            } else {
                ready = false;
            }
        }
        Ok(ready)
    }
}
fn recyclable(image: &Rc<Image>, item: &crate::common::image_plan::Required<'_>) -> bool {
    Rc::strong_count(image) == 1
        && image
            .source
            .as_ref()
            .is_some_and(|source| source.lease.is_released())
        && image.kind == 0.
        && !image.coverage
        && match item.source.data() {
            ResourceData::Image { image: source, .. } => {
                image.format.is_none() && image.layout.storage_size == [source.width, source.height]
            }
            ResourceData::Texture { descriptor, .. } => {
                image.format == Some(descriptor.format)
                    && image.layout.storage_size
                        == item.texture.expect("planned texture").layout.storage_size
            }
            _ => false,
        }
}

fn progress(
    gpu: &native::Context,
    budgets: &Budgets,
    limits: crate::common::GpuLimits,
    pending: &mut Pending,
    quota: &mut u64,
    scratch: &mut crate::common::resources::UploadScratch,
) -> Result<bool, GlesError> {
    loop {
        let (size, stride, pixels, convert, alpha) = match pending.source.data() {
            ResourceData::Image { image, .. } => (
                [image.width, image.height],
                image.width as usize * 4,
                image.data.data(),
                true,
                image.alpha_type,
            ),
            ResourceData::Texture { descriptor, bytes } => (
                pending.image.layout.storage_size,
                descriptor
                    .format
                    .row_bytes(descriptor.layout.storage_size[0])
                    .ok_or(GlesError::InvalidSize)? as usize,
                bytes.data(),
                false,
                ImageAlphaType::AlphaPremultiplied,
            ),
            ResourceData::Yuv420(image) => {
                if pending.plane >= image.plane_count() {
                    return Ok(true);
                }
                (
                    image.plane_size(pending.plane),
                    image.row_stride(pending.plane) as usize,
                    image.plane(pending.plane),
                    false,
                    ImageAlphaType::AlphaPremultiplied,
                )
            }
            _ => unreachable!(),
        };
        if pending.row == size[1] {
            if pending.image.kind != 0. {
                pending.plane += 1;
                pending.row = 0;
                continue;
            }
            return Ok(true);
        }
        if *quota == 0 {
            return Ok(false);
        }
        let row = if let ResourceData::Yuv420(image) = pending.source.data() {
            image.row_bytes(pending.plane) as usize
        } else if let Some(format) = pending.image.format {
            format.row_bytes(size[0]).ok_or(GlesError::InvalidSize)? as usize
        } else {
            stride
        };
        let rows = ((*quota / row as u64).max(1).min(u64::from(u32::MAX)) as u32)
            .min(size[1] - pending.row);
        let texture = if pending.plane == 0 {
            &pending.image.texture
        } else {
            &pending.image.planes[pending.plane - 1]
        };
        if convert || stride != row {
            let available = limits
                .transient_bytes
                .saturating_sub(budgets.transient.usage().bytes)
                .max(scratch.capacity());
            let rows = rows.min((available / row as u64).max(1).min(u64::from(u32::MAX)) as u32);
            let bytes = rows as usize * row;
            let data = scratch.get(&budgets.transient, limits.transient_bytes, bytes)?;
            let start = pending.row as usize * stride;
            if convert {
                for (source, target) in pixels[start..start + rows as usize * stride]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(data.as_chunks_mut::<4>().0.iter_mut())
                {
                    let linear = crate::common::resources::rgba8_linear_u8(
                        source,
                        pending.image.encoding,
                        alpha,
                    );
                    target.copy_from_slice(&linear);
                }
            } else {
                for (y, target) in data.chunks_exact_mut(row).enumerate() {
                    target.copy_from_slice(&pixels[start + y * stride..start + y * stride + row]);
                }
            }
            gpu.upload(texture, [0, pending.row], [size[0], rows], data, row)?;
            pending.row += rows;
            *quota = quota.saturating_sub(bytes as u64);
        } else {
            let start = pending.row as usize * stride;
            let span = (rows as usize - 1) * stride + row;
            gpu.upload(
                texture,
                [0, pending.row],
                [size[0], rows],
                &pixels[start..start + span],
                stride,
            )?;
            pending.row += rows;
            *quota = quota.saturating_sub(u64::from(rows) * row as u64);
        }
    }
}
pub(super) fn reserve(
    pool: &BudgetPool,
    bytes: u64,
    limit: u64,
    domain: &'static str,
) -> Result<BudgetTicket, GlesError> {
    if bytes > limit {
        return Err(GlesError::Budget {
            domain,
            requested: bytes,
            available: limit,
        });
    }
    pool.reserve(bytes).map_err(|_| GlesError::Pressure {
        domain,
        requested: bytes,
        available: limit.saturating_sub(pool.usage().bytes),
    })
}

#[cfg(test)]
mod retirement_tests {
    use super::*;
    use reel_core::{
        budget::{BudgetCategory, OwnedBytes, QueueLimits},
        identity::{AssetKey, ContentVersion},
        texture::{TextureDescriptor, TexturePayload},
    };
    use reel_ui::resources::ResourceRegistry;

    #[test]
    #[ignore = "requires host EGL and BC support; uploads without pixel readback"]
    fn retired_native_texture_storage_reuses_exact_footprints_and_budget() {
        let gpu = native::Context::headless([8, 8], false).unwrap();
        assert!(gpu.capabilities().bc);
        let limits = crate::common::GpuLimits::default();
        let budgets = Budgets::new(limits);
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 128,
                max_bytes: 1 << 20,
            },
        );
        let owner =
            reel_ui::core::WidgetPod::new(reel_ui::widgets::Label::new("resource test")).id();
        let mut images = Images::default();
        for format in [
            None,
            Some(TextureFormat::Rgba8),
            Some(TextureFormat::Bc1Rgb),
            Some(TextureFormat::Bc3Rgba),
        ] {
            let mut identity = None;
            for version in 0..8u8 {
                let resource = ResourceVersion {
                    key: AssetKey(format!("native-reuse-{format:?}")),
                    version: ContentVersion([version; 32]),
                };
                let descriptor = TextureDescriptor {
                    format: format.unwrap_or(TextureFormat::Rgba8),
                    source_encoding: if version % 2 == 0 {
                        ColorEncoding::Linear
                    } else {
                        ColorEncoding::Srgb
                    },
                    layout: TextureLayout {
                        logical_size: [16 + u32::from(version), 16],
                        content_size: [8, 8],
                        storage_size: [8, 8],
                    },
                };
                let length = descriptor.byte_len().unwrap();
                let bytes =
                    OwnedBytes::new(vec![255; length as usize], pool.reserve(length).unwrap())
                        .unwrap();
                let mut registry = ResourceRegistry::new(1, 65536).unwrap();
                let handle = if format.is_some() {
                    registry
                        .register_texture(
                            resource.clone(),
                            TexturePayload::new(descriptor, bytes).unwrap(),
                        )
                        .unwrap()
                } else {
                    registry
                        .register_image(
                            resource.clone(),
                            bytes,
                            [8, 8],
                            ImageFormat::Rgba8,
                            ImageAlphaType::Alpha,
                            descriptor.source_encoding,
                        )
                        .unwrap()
                };
                let source = registry.acquire(handle).unwrap();
                let item = crate::common::image_plan::Required {
                    source: &source,
                    at: reel_ui::frame::RecordLocation {
                        fragment: 0,
                        command: 0,
                        mask_depth: 0,
                        owner,
                    },
                    texture: format.map(|_| {
                        crate::common::resources::TextureUploadLayout::new(descriptor).unwrap()
                    }),
                };
                let mut pending = images.allocate(&gpu, &budgets, limits, &item).unwrap();
                if !format.is_some_and(TextureFormat::is_compressed) {
                    let mut quota = u64::MAX;
                    assert!(
                        progress(
                            &gpu,
                            &budgets,
                            limits,
                            &mut pending,
                            &mut quota,
                            &mut images.scratch
                        )
                        .unwrap()
                    );
                }
                if let Some(texture) = &identity {
                    assert!(texture == &pending.image.texture);
                } else {
                    identity = Some(pending.image.texture.clone());
                }
                assert_eq!(pending.image.encoding, descriptor.source_encoding);
                assert_eq!(
                    pending.image.layout,
                    if format.is_some() {
                        descriptor.layout
                    } else {
                        TextureLayout::identity([8, 8])
                    }
                );
                images.entries.insert(handle, pending.image.clone());
                gpu.finish().unwrap();
                registry.retire(handle);
                images.prune(&[resource]);
                drop(pending);
                drop(source);
                drop(registry);
                assert_eq!(budgets.resources.usage().bytes, length);
            }
            drop(identity);
            images.release_idle();
            assert_eq!(budgets.resources.usage().bytes, 0);
            assert_eq!(budgets.transient.usage().bytes, 0);
            assert_eq!(pool.usage().bytes, 0);
            eprintln!(
                "native {format:?}: 8 resource versions retain one texture identity/reservation; cache clear releases all bytes"
            );
        }
    }
}
