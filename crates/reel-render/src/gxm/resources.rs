pub(super) use super::programs::Target;
use super::*;
use crate::common::resources::ResourceSource;
use reel_core::texture::{TextureFormat, TextureLayout};
use reel_ui::{
    peniko::{ImageAlphaType, ImageFormat},
    resources::{ResourceData, ResourcePixels},
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
    native_import: bool,
    reuse_count: u8,
    pub source: Option<ResourceSource>,
    _storage: super::ownership::NativeTicket,
}
impl Image {
    pub fn uv_transform(&self) -> reel_ui::kurbo::Affine {
        let [x, y] = self.layout.uv_scale();
        reel_ui::kurbo::Affine::scale_non_uniform(x, y)
    }
}

struct Pending {
    source: ResourceLease,
    pixels: Option<ResourcePixels>,
    _metadata: BudgetTicket,
    image: Rc<Image>,
    plane: usize,
    row: u32,
}
#[derive(Default)]
pub(super) struct Images {
    pub entries: HashMap<ResourceHandle, Rc<Image>>,
    pending: HashMap<ResourceHandle, Pending>,
    white: Option<Rc<Image>>,
    failures: Vec<super::ownership::NativeTicket>,
    scratch: crate::common::resources::UploadScratch,
    retired: crate::common::resources::RetiredImages<Rc<Image>>,
}
impl Images {
    pub fn submitted(&mut self) {
        self.retired.submitted();
    }
    pub fn usage(&self) -> [u64; 3] {
        let cached = self
            .entries
            .values()
            .map(|image| image._storage.bytes())
            .sum();
        let mut retired = 0;
        let mut readers = 0;
        for image in self.retired.iter() {
            let bytes = image._storage.bytes();
            retired += bytes;
            if Rc::strong_count(image) > 1
                || image
                    .source
                    .as_ref()
                    .is_some_and(|source| !source.lease.is_released())
            {
                readers += bytes;
            }
        }
        [cached, retired, readers]
    }
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
    ) -> Result<u64, GpuError> {
        let mut resident = 0u64;
        let mut additional = 0u64;
        let mut staging = 0;
        let mut pending_staging = 0;
        let mut claimed = [false; 64];
        for item in required {
            if let ResourceData::Texture { descriptor, .. } = item.source.data() {
                if match descriptor.format {
                    TextureFormat::Bc1Rgb | TextureFormat::Bc3Rgba => !caps.bc,
                    TextureFormat::Bgra8 => !caps.bgra,
                    TextureFormat::LinearRgba16Float => !caps.half_float,
                    TextureFormat::Rgba8 => false,
                } {
                    return Err(GpuError::Unsupported {
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
                    return Err(GpuError::Unsupported {
                        at: item.at,
                        record: "native BC texture dimensions",
                    });
                }
            }
            let (bytes, upload) = costs(item)?;
            resident = resident.checked_add(bytes).ok_or(GpuError::InvalidSize)?;
            if !self.entries.contains_key(&item.source.handle()) {
                pending_staging = pending_staging.max(upload);
                if !self.pending.contains_key(&item.source.handle()) {
                    let reusable =
                        self.retired.iter().enumerate().find(|(index, image)| {
                            !claimed[*index] && recyclable(image, item, bytes)
                        });
                    if let Some((index, _)) = reusable {
                        claimed[index] = true;
                    } else {
                        additional = additional.checked_add(bytes).ok_or(GpuError::InvalidSize)?;
                    }
                }
            }
            staging = staging.max(upload);
        }
        if staging > limits.transient_bytes {
            return Err(GpuError::Budget {
                domain: "image upload staging",
                requested: staging,
                available: limits.transient_bytes,
            });
        }
        crate::common::image_plan::peak(std::iter::once((resident, 0)), limits.resource_bytes)?;
        crate::common::image_plan::peak(std::iter::once((resident, staging)), total_bytes)?;
        additional
            .checked_add(pending_staging)
            .ok_or(GpuError::InvalidSize)
    }
    pub fn prune(&mut self, versions: &[ResourceVersion]) {
        for (_, image) in self.entries.extract_if(|_, image| {
            !image
                .source
                .as_ref()
                .is_some_and(|source| source.retained(versions))
        }) {
            if !image.coverage && !image.native_import {
                let uses = image.reuse_count;
                self.retired.put(image, uses);
            }
        }
        self.retired.prune_cold(reusable);
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
        gpu: &super::ownership::Device,
        budgets: &Budgets,
        limit: u64,
    ) -> Result<Rc<Image>, GpuError> {
        if let Some(image) = &self.white {
            return Ok(image.clone());
        }
        let bytes = super::super::texture_bytes([1, 1], native::TextureFormat::Rgba8)?;
        let storage = reserve(&budgets.resources, bytes, limit, "white texture")?;
        let storage = gpu.ticket(storage);
        let built = (|| {
            let texture = gpu.create_texture(
                [1, 1],
                native::TextureFormat::Rgba8,
                native::TextureMemory::Cdram,
            )?;
            gpu.upload(&texture, [0, 0], [1, 1], &[255; 4], 4)?;
            Ok::<_, GpuError>(texture)
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
            native_import: false,
            reuse_count: 0,
            source: None,
            _storage: storage,
        });
        self.white = Some(image.clone());
        Ok(image)
    }
    fn allocate(
        &mut self,
        gpu: &super::ownership::Device,
        budgets: &Budgets,
        limits: crate::common::GpuLimits,
        item: &crate::common::image_plan::Required<'_>,
    ) -> Result<Pending, GpuError> {
        let source = item.source;
        let pixels = source.read_pixels();
        let metadata = reserve(
            &budgets.transient,
            (size_of::<Pending>() + 4 * size_of::<ResourceHandle>()) as u64,
            limits.transient_bytes,
            "partial image metadata",
        )?;
        let at = item.at;
        let native_import =
            matches!(source.data(), ResourceData::Yuv420(image) if native_frame(image).is_some());
        let (bytes, _) = costs(item)?;
        let (layout, format, encoding, kind) = if let Some(image) = source.image_descriptor() {
            if image.format != ImageFormat::Rgba8 {
                return Err(GpuError::Unsupported {
                    at,
                    record: "image format",
                });
            }
            (
                TextureLayout::identity([image.width, image.height]),
                None,
                image.encoding,
                0.,
            )
        } else if let Some(descriptor) = source.texture_descriptor() {
            let upload = item.texture.expect("planned texture");
            (
                upload.layout,
                Some(descriptor.format),
                descriptor.source_encoding,
                0.,
            )
        } else if let ResourceData::Yuv420(image) = source.data() {
            (
                TextureLayout::identity(image.size()),
                None,
                ColorEncoding::Linear,
                if image.layout() == reel_core::image::Yuv420Layout::Nv12 {
                    2.
                } else {
                    1.
                },
            )
        } else {
            return Err(GpuError::Unsupported {
                at,
                record: "non-image upload",
            });
        };
        let mip_count = source
            .texture_descriptor()
            .map_or(1, |descriptor| descriptor.mip_count);
        let recycled = self
            .retired
            .take_if(|image| recyclable(image, item, bytes))
            .and_then(Rc::into_inner);
        let (storage, recycled_textures, reuse_count) = if let Some(image) = recycled {
            (
                image._storage,
                Some((image.texture, image.planes)),
                image.reuse_count.saturating_add(1),
            )
        } else {
            self.make_room(&budgets.resources, bytes, limits.resource_bytes);
            (
                gpu.ticket(reserve(
                    &budgets.resources,
                    bytes,
                    limits.resource_bytes,
                    "image residency",
                )?),
                None,
                0,
            )
        };
        let built = (|| {
            let mut planes = Vec::new();
            let mut video = [[0.; 4]; 4];
            let texture = if let ResourceData::Yuv420(image) = source.data() {
                video[..3].copy_from_slice(&image.rgb_rows());
                video[3][0] =
                    f32::from(image.color().transfer == reel_core::image::YuvTransfer::Srgb);
                if let Some(shared) = native_frame(image) {
                    let [y, uv] = gpu.import_nv12(shared)?;
                    planes.push(uv);
                    y
                } else if let Some((texture, chroma)) = recycled_textures {
                    planes = chroma;
                    texture
                } else {
                    for index in 0..image.plane_count() {
                        planes.push(gpu.create_texture(
                            image.plane_size(index),
                            plane_format(image, index),
                            native::TextureMemory::Main,
                        )?);
                    }
                    planes.remove(0)
                }
            } else if format.is_some_and(TextureFormat::is_compressed) {
                // Compressed allocation is inseparable from its complete mip upload.
                let ResourceData::Texture { descriptor, .. } = source.data() else {
                    unreachable!()
                };
                let blocks = pixels.as_ref().expect("texture pixel reader");
                let texture = if let Some((texture, _)) = recycled_textures {
                    texture
                } else {
                    gpu.create_mip_texture(
                        layout.storage_size,
                        native_format(format.unwrap()),
                        mip_count,
                        native::TextureMemory::Main,
                    )?
                };
                match descriptor.storage_layout {
                    reel_core::texture::TextureStorageLayout::Linear => {
                        gpu.upload_bc_texture(&texture, blocks.bytes())?
                    }
                    reel_core::texture::TextureStorageLayout::GxmBcMorton => {
                        gpu.upload_bc_morton_texture(&texture, blocks.bytes())?
                    }
                }
                texture
            } else {
                if let Some((texture, _)) = recycled_textures {
                    texture
                } else {
                    gpu.create_mip_texture(
                        layout.storage_size,
                        native_format(format.unwrap_or(TextureFormat::Rgba8)),
                        mip_count,
                        native::TextureMemory::Main,
                    )?
                }
            };
            Ok::<_, GpuError>((texture, planes, video))
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
            native_import,
            reuse_count,
            source: Some(ResourceSource {
                version: source.version().clone(),
                lease: source.downgrade(),
            }),
            _storage: storage,
        });
        Ok(Pending {
            source: source.clone(),
            pixels,
            _metadata: metadata,
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
    ) -> Result<bool, GpuError> {
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
fn native_format(format: TextureFormat) -> native::TextureFormat {
    match format {
        TextureFormat::Rgba8 => native::TextureFormat::Rgba8,
        TextureFormat::Bgra8 => native::TextureFormat::Bgra8,
        TextureFormat::LinearRgba16Float => native::TextureFormat::Rgba16Float,
        TextureFormat::Bc1Rgb => native::TextureFormat::Bc1Rgb,
        TextureFormat::Bc3Rgba => native::TextureFormat::Bc3Rgba,
    }
}

fn plane_format(image: &reel_core::image::Yuv420Image, index: usize) -> native::TextureFormat {
    if index == 1 && image.layout() == reel_core::image::Yuv420Layout::Nv12 {
        native::TextureFormat::LumaAlpha8
    } else {
        native::TextureFormat::Luma8
    }
}

/// Use the same native footprint for admission and allocation. CPU upload rows
/// retain their portable stride, independently of the mapped texture pitch.
fn costs(item: &crate::common::image_plan::Required<'_>) -> Result<(u64, u64), GpuError> {
    match item.source.data() {
        ResourceData::Image { image, .. } => Ok((
            super::super::texture_bytes([image.width, image.height], native::TextureFormat::Rgba8)?,
            u64::from(image.width) * 4,
        )),
        ResourceData::Texture { descriptor, .. } => {
            let upload = item.texture.expect("planned texture");
            Ok((
                native::mip_texture_layout(
                    upload.layout.storage_size,
                    native_format(descriptor.format),
                    descriptor.mip_count,
                )?
                .bytes as u64,
                0,
            ))
        }
        ResourceData::Yuv420(image) => {
            if native_frame(image).is_some() {
                return Ok((0, 0));
            }
            let mut bytes = 0_u64;
            for index in 0..image.plane_count() {
                let plane = super::super::texture_bytes(
                    image.plane_size(index),
                    plane_format(image, index),
                )?;
                bytes = bytes.checked_add(plane).ok_or(GpuError::InvalidSize)?;
            }
            Ok((bytes, 0))
        }
        _ => unreachable!("common image demand validates resource kinds"),
    }
}

fn reusable(image: &Rc<Image>) -> bool {
    Rc::strong_count(image) == 1
        && image
            .source
            .as_ref()
            .is_some_and(|source| source.lease.is_released())
}

fn recyclable(
    image: &Rc<Image>,
    item: &crate::common::image_plan::Required<'_>,
    bytes: u64,
) -> bool {
    // Flights and prepared frames retain their Image references through the
    // completion fence. The final reader and source lease must both be gone.
    reusable(image)
        && image._storage.bytes() == bytes
        && !image.coverage
        && !image.native_import
        && !matches!(item.source.data(), ResourceData::Yuv420(source) if native_frame(source).is_some())
        && match item.source.data() {
            ResourceData::Image { image: source, .. } => {
                image.kind == 0.
                    && image.format.is_none()
                    && image.layout.storage_size == [source.width, source.height]
            }
            ResourceData::Texture { descriptor, .. } => {
                image.kind == 0.
                    && image.format == Some(descriptor.format)
                    && image.texture.mip_count() == descriptor.mip_count
                    && image.layout.storage_size
                        == item.texture.expect("planned texture").layout.storage_size
            }
            ResourceData::Yuv420(source) => {
                image.layout.storage_size == source.size()
                    && image.planes.len() + 1 == source.plane_count()
                    && image.kind
                        == if source.layout() == reel_core::image::Yuv420Layout::Nv12 {
                            2.
                        } else {
                            1.
                        }
            }
            _ => false,
        }
}

fn progress(
    gpu: &super::ownership::Device,
    budgets: &Budgets,
    limits: crate::common::GpuLimits,
    pending: &mut Pending,
    quota: &mut u64,
    scratch: &mut crate::common::resources::UploadScratch,
) -> Result<bool, GpuError> {
    if pending.image.native_import {
        return Ok(true);
    }
    loop {
        let (size, stride, pixels, convert, alpha) =
            if let Some(image) = pending.source.image_descriptor() {
                (
                    [image.width, image.height],
                    image.width as usize * 4,
                    pending.pixels.as_ref().expect("image pixel reader").bytes(),
                    true,
                    image.alpha_type,
                )
            } else if let Some(descriptor) = pending.source.texture_descriptor() {
                if pending.plane >= descriptor.mip_count as usize {
                    return Ok(true);
                }
                let size = if descriptor.mip_count == 1 {
                    pending.image.layout.storage_size
                } else {
                    descriptor.mip_size(pending.plane as u32)
                };
                (
                    size,
                    descriptor
                        .format
                        .row_bytes(descriptor.mip_size(pending.plane as u32)[0])
                        .ok_or(GpuError::InvalidSize)? as usize,
                    &pending
                        .pixels
                        .as_ref()
                        .expect("texture pixel reader")
                        .bytes()[descriptor
                        .mip_offset(pending.plane as u32)
                        .ok_or(GpuError::InvalidSize)? as usize..],
                    false,
                    ImageAlphaType::AlphaPremultiplied,
                )
            } else if let ResourceData::Yuv420(image) = pending.source.data() {
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
            } else {
                unreachable!()
            };
        if pending.row == size[1] {
            if pending.image.kind != 0. || pending.source.texture_descriptor().is_some() {
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
            format.row_bytes(size[0]).ok_or(GpuError::InvalidSize)? as usize
        } else {
            stride
        };
        let rows = ((*quota / row as u64).max(1).min(u64::from(u32::MAX)) as u32)
            .min(size[1] - pending.row);
        let texture = if pending.plane == 0 || pending.source.texture_descriptor().is_some() {
            &pending.image.texture
        } else {
            &pending.image.planes[pending.plane - 1]
        };
        if convert {
            let available = limits
                .transient_bytes
                .saturating_sub(budgets.transient.usage().bytes)
                .max(scratch.capacity());
            let rows = rows.min((available / row as u64).max(1).min(u64::from(u32::MAX)) as u32);
            let bytes = rows as usize * row;
            let data = scratch.get(&budgets.transient, limits.transient_bytes, bytes)?;
            let start = pending.row as usize * stride;
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
            gpu.upload(texture, [0, pending.row], [size[0], rows], data, row)?;
            pending.row += rows;
            *quota = quota.saturating_sub(bytes as u64);
        } else {
            let start = pending.row as usize * stride;
            let span = (rows as usize - 1) * stride + row;
            if pending
                .source
                .texture_descriptor()
                .is_some_and(|descriptor| descriptor.mip_count > 1)
            {
                gpu.upload_mip_rows(
                    texture,
                    pending.plane as u32,
                    pending.row,
                    rows,
                    &pixels[start..start + span],
                    stride,
                )?;
            } else {
                gpu.upload(
                    texture,
                    [0, pending.row],
                    [size[0], rows],
                    &pixels[start..start + span],
                    stride,
                )?;
            }
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
) -> Result<BudgetTicket, GpuError> {
    if bytes > limit {
        return Err(GpuError::Budget {
            domain,
            requested: bytes,
            available: limit,
        });
    }
    pool.reserve(bytes).map_err(|_| GpuError::Pressure {
        domain,
        requested: bytes,
        available: limit.saturating_sub(pool.usage().bytes),
    })
}

fn native_frame(image: &reel_core::image::Yuv420Image) -> Option<&vita_native::video::SharedNv12> {
    let shared = image
        .native_storage()?
        .downcast_ref::<vita_native::video::SharedNv12>()?;
    (image.layout() == reel_core::image::Yuv420Layout::Nv12
        && image.size() == shared.size()
        && (0..2).all(|index| {
            image.row_stride(index) == shared.strides()[index]
                && std::ptr::eq(image.plane(index), shared.plane(index))
        }))
    .then_some(shared)
}
