pub use crate::common::{GpuError, GpuLimits, SurfaceFailure};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceGeneration(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quality {
    pub format: wgpu::TextureFormat,
    pub samples: u32,
    pub degraded: bool,
}
#[derive(Debug)]
pub struct DeviceLoss {
    pub reason: wgpu::DeviceLostReason,
    pub message: String,
}

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter: wgpu::Adapter,
    pub(super) generation: DeviceGeneration,
    pub(super) limits: GpuLimits,
    lost: Arc<Mutex<Option<Arc<DeviceLoss>>>>,
    fault: Arc<Mutex<Option<Arc<wgpu::Error>>>>,
    #[cfg(test)]
    pub(super) upload_creations: super::resources::UploadCreations,
}
impl Gpu {
    pub(super) fn submit(&self, encoder: wgpu::CommandEncoder) {
        let _submit = reel_core::profile::Scope::new(reel_core::profile::Phase::RenderFence);
        let command = {
            let _finish = reel_core::profile::Scope::new(reel_core::profile::Phase::CommandFinish);
            encoder.finish()
        };
        self.queue.submit([command]);
    }
    pub fn generation(&self) -> DeviceGeneration {
        self.generation
    }
    pub fn limits(&self) -> GpuLimits {
        self.limits
    }
    /// Request a native renderer device. The instance must leave
    /// `STRICT_WEBGPU_COMPLIANCE` unset: fully overwritten scratch attachments
    /// use WGPU's `DontCare` load operation.
    pub async fn request(
        instance: &wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
        generation: DeviceGeneration,
        limits: GpuLimits,
    ) -> Result<Self, GpuError> {
        if generation.0 == 0 || limits.max_inflight == 0 {
            return Err(GpuError::IdentityExhausted);
        }
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: surface,
                power_preference: wgpu::PowerPreference::None,
                force_fallback_adapter: false,
                ..Default::default()
            })
            .await
            .map_err(|e| GpuError::Adapter(e.to_string()))?;
        let memory_hints = if adapter.get_info().backend == wgpu::Backend::Vulkan {
            // Bound individual driver allocations when scratch targets and
            // sampled images share the same Vulkan allocator.
            const BLOCK_BYTES: u64 = 64 << 20;
            wgpu::MemoryHints::Manual {
                suballocated_device_memory_block_size: BLOCK_BYTES..BLOCK_BYTES,
            }
        } else {
            wgpu::MemoryHints::Performance
        };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("reel device"),
                required_features: adapter.features()
                    & (wgpu::Features::TEXTURE_COMPRESSION_BC | wgpu::Features::SHADER_F16),
                required_limits: wgpu::Limits::default(),
                memory_hints,
                trace: wgpu::Trace::Off,
                ..Default::default()
            })
            .await
            .map_err(|e| GpuError::Device(e.to_string()))?;
        let lost = Arc::new(Mutex::new(None));
        let flag = lost.clone();
        device.set_device_lost_callback(move |reason, message| {
            *flag.lock().expect("device loss state poisoned") =
                Some(Arc::new(DeviceLoss { reason, message }));
        });
        let fault = Arc::new(Mutex::new(None));
        let capture = fault.clone();
        device.on_uncaptured_error(Arc::new(move |error| {
            let mut first = capture.lock().expect("device error state poisoned");
            if first.is_none() {
                *first = Some(Arc::new(error));
            }
        }));
        Ok(Self {
            device,
            queue,
            adapter,
            generation,
            limits,
            lost,
            fault,
            #[cfg(test)]
            upload_creations: Default::default(),
        })
    }
    pub fn check(&self) -> Result<(), GpuError> {
        if let Some(loss) = &*self.lost.lock().expect("device loss state poisoned") {
            Err(GpuError::DeviceLost(loss.clone()))
        } else if let Some(error) = &*self.fault.lock().expect("device error state poisoned") {
            Err(GpuError::Driver(error.clone()))
        } else {
            Ok(())
        }
    }
    /// Constructor-owned scopes must publish their errors to the same sticky
    /// state as uncaptured errors, even when a caller has an outer scope.
    pub(super) fn capture_driver_error(&self, error: wgpu::Error) {
        let mut first = self.fault.lock().expect("device error state poisoned");
        if first.is_none() {
            *first = Some(Arc::new(error));
        }
    }
    /// Capability probe for one target. Renderer negotiates the complete frame's peak.
    pub fn quality(&self, size: [u32; 2]) -> Result<Quality, GpuError> {
        self.quality_for_targets(size, |quality| super::targets::target_bytes(size, quality))
            .map(|(quality, _)| quality)
    }
    pub(super) fn quality_for_targets(
        &self,
        size: [u32; 2],
        mut target_cost: impl FnMut(Quality) -> Result<u64, GpuError>,
    ) -> Result<(Quality, u64), GpuError> {
        self.check()?;
        let max = self.device.limits().max_texture_dimension_2d;
        if size.contains(&0) || size.iter().any(|&v| v > max) {
            return Err(GpuError::InvalidSize);
        }
        let mut bytes = 0;
        for q in self.supported_qualities() {
            bytes = target_cost(q)?;
            if bytes <= self.limits.target_bytes {
                return Ok((q, bytes));
            }
        }
        Err(GpuError::Budget {
            domain: "targets",
            requested: bytes,
            available: self.limits.target_bytes,
        })
    }
    /// Match the existing color attachment's channel precision and sampling.
    pub(super) fn coverage_quality(&self, color: Quality) -> Option<Quality> {
        let format = match color.format {
            wgpu::TextureFormat::Rgba8Unorm => wgpu::TextureFormat::R8Unorm,
            wgpu::TextureFormat::Rgba16Float => wgpu::TextureFormat::R16Float,
            _ => return None,
        };
        let features = self.adapter.get_texture_format_features(format);
        let usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        if !features.allowed_usages.contains(usage)
            || !features.flags.contains(
                wgpu::TextureFormatFeatureFlags::FILTERABLE
                    | wgpu::TextureFormatFeatureFlags::BLENDABLE,
            )
            || !features.flags.sample_count_supported(color.samples)
            || (color.samples > 1
                && !features
                    .flags
                    .contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_RESOLVE))
        {
            return None;
        }
        Some(Quality { format, ..color })
    }
    pub(super) fn supported_qualities(&self) -> impl Iterator<Item = Quality> + '_ {
        [
            (wgpu::TextureFormat::Rgba16Float, 4),
            (wgpu::TextureFormat::Rgba16Float, 1),
            (wgpu::TextureFormat::Rgba8Unorm, 1),
        ]
        .into_iter()
        .filter_map(|(format, samples)| {
            let features = self.adapter.get_texture_format_features(format);
            let usage =
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
            if !features.allowed_usages.contains(usage)
                || !features
                    .flags
                    .contains(wgpu::TextureFormatFeatureFlags::BLENDABLE)
                || !features.flags.sample_count_supported(samples)
            {
                return None;
            }
            Some(Quality {
                format,
                samples,
                degraded: samples != 4 || format != wgpu::TextureFormat::Rgba16Float,
            })
        })
    }
}

impl From<super::surface::ContractError> for GpuError {
    fn from(error: super::surface::ContractError) -> Self {
        Self::SurfaceContract(error)
    }
}
