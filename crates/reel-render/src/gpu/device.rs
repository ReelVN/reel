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
    pub fn generation(&self) -> DeviceGeneration {
        self.generation
    }
    pub fn limits(&self) -> GpuLimits {
        self.limits
    }
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
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("reel device"),
                required_features: adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC,
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
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
        self.quality_for_targets(size, |_| Ok(1))
            .map(|(quality, _)| quality)
    }
    pub(super) fn quality_for_targets(
        &self,
        size: [u32; 2],
        mut target_count: impl FnMut(u32) -> Result<u64, GpuError>,
    ) -> Result<(Quality, u64), GpuError> {
        self.check()?;
        let max = self.device.limits().max_texture_dimension_2d;
        if size.contains(&0) || size.iter().any(|&v| v > max) {
            return Err(GpuError::InvalidSize);
        }
        let mut targets = 1;
        for q in self.supported_qualities() {
            targets = target_count(q.samples)?;
            if super::targets::target_bytes(size, q)?
                .checked_mul(targets)
                .is_some_and(|bytes| bytes <= self.limits.target_bytes)
            {
                return Ok((q, targets));
            }
        }
        Err(GpuError::Budget {
            domain: "targets",
            requested: (u64::from(size[0]) * u64::from(size[1]) * 4).saturating_mul(targets),
            available: self.limits.target_bytes,
        })
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
