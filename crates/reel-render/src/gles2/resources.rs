pub(super) use super::programs::Target;
use super::*;
use reel_core::texture::{TextureFormat, TextureLayout};
use reel_ui::{
    frame::RecordLocation,
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
    pub source: Option<ResourceWeak>,
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
}
impl Images {
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
                    let bytes = descriptor.byte_len().map_err(|_| GlesError::InvalidSize)?;
                    (
                        bytes,
                        if descriptor.format.is_compressed() {
                            bytes
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
                    additional = additional
                        .checked_add(bytes)
                        .ok_or(GlesError::InvalidSize)?;
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
        self.entries.retain(|_, image| {
            image.source.as_ref().is_some_and(|source| {
                source
                    .upgrade()
                    .is_some_and(|lease| !lease.is_retired() && !versions.contains(lease.version()))
            })
        });
        self.pending.retain(|_, pending| {
            !pending.source.is_retired() && !versions.contains(pending.source.version())
        });
    }
    fn make_room(&mut self, budget: &BudgetPool, cost: u64, limit: u64) {
        if cost > limit.saturating_sub(budget.usage().bytes) {
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
        source: &ResourceLease,
        at: RecordLocation,
    ) -> Result<Pending, GlesError> {
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
            ResourceData::Texture { descriptor, .. } => (
                descriptor.layout,
                Some(descriptor.format),
                descriptor.source_encoding,
                0.,
                descriptor.byte_len().map_err(|_| GlesError::InvalidSize)?,
            ),
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
        self.make_room(&budgets.resources, bytes, limits.resource_bytes);
        let storage = reserve(
            &budgets.resources,
            bytes,
            limits.resource_bytes,
            "image residency",
        )?;
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
                let _staging = reserve(
                    &budgets.transient,
                    bytes,
                    limits.transient_bytes,
                    "compressed upload staging",
                )?;
                let mut staging =
                    vec![0; usize::try_from(bytes).map_err(|_| GlesError::InvalidSize)?];
                gpu.create_bc_texture(
                    layout.storage_size,
                    if format == Some(TextureFormat::Bc1Rgb) {
                        native::TextureFormat::Bc1Rgb
                    } else {
                        native::TextureFormat::Bc3Rgba
                    },
                    blocks.data(),
                    &mut staging,
                )?
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
            source: Some(source.downgrade()),
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
        gpu: &native::Context,
        budgets: &Budgets,
        limits: crate::common::GpuLimits,
        required: &[crate::common::image_plan::Required<'_>],
        quota: &mut u64,
        output: &mut HashMap<ResourceHandle, Rc<Image>>,
        touched: &mut Vec<Rc<Image>>,
    ) -> Result<bool, GlesError> {
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
                let pending = self.allocate(gpu, budgets, limits, source, item.at)?;
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
            let complete = progress(gpu, budgets, limits, pending, quota)?;
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
fn progress(
    gpu: &native::Context,
    budgets: &Budgets,
    limits: crate::common::GpuLimits,
    pending: &mut Pending,
    quota: &mut u64,
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
                descriptor.layout.storage_size,
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
                .saturating_sub(budgets.transient.usage().bytes);
            let rows = rows.min((available / row as u64).max(1).min(u64::from(u32::MAX)) as u32);
            let bytes = rows as usize * row;
            let _staging = reserve(
                &budgets.transient,
                bytes as u64,
                limits.transient_bytes,
                "linear image staging",
            )?;
            let mut data = vec![0; bytes];
            let start = pending.row as usize * stride;
            if convert {
                for (source, target) in pixels[start..start + rows as usize * stride]
                    .chunks_exact(4)
                    .zip(data.chunks_exact_mut(4))
                {
                    let linear = crate::common::resources::rgba8_linear_f16(
                        source,
                        pending.image.encoding,
                        alpha,
                    );
                    for i in 0..4 {
                        target[i] = (half::f16::from_bits(linear[i]).to_f32().clamp(0., 1.) * 255.)
                            .round() as u8;
                    }
                }
            } else {
                for (y, target) in data.chunks_exact_mut(row).enumerate() {
                    target.copy_from_slice(&pixels[start + y * stride..start + y * stride + row]);
                }
            }
            gpu.upload(texture, [0, pending.row], [size[0], rows], &data, row)?;
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
