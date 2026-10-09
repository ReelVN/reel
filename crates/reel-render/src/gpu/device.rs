use reel_ui::frame::RecordLocation;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceGeneration(pub u64);
#[derive(Clone, Copy, Debug)]
pub struct GpuLimits {
    pub target_bytes: u64,
    pub resource_bytes: u64,
    pub transient_bytes: u64,
    pub max_inflight: usize,
    pub max_depth: usize,
}
impl Default for GpuLimits {
    fn default() -> Self {
        Self {
            target_bytes: 128 << 20,
            resource_bytes: 128 << 20,
            transient_bytes: 32 << 20,
            max_inflight: 2,
            max_depth: 32,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quality {
    pub format: wgpu::TextureFormat,
    pub samples: u32,
    pub degraded: bool,
}
#[derive(Debug)]
pub enum GpuError {
    Adapter(String),
    Device(String),
    DeviceLost,
    Driver(Arc<wgpu::Error>),
    InvalidSize,
    IdentityExhausted,
    Busy,
    /// Other prepared/in-flight submissions occupy quota needed by this request.
    /// Later records can still exceed the preparation's own quota on retry.
    Pressure {
        domain: &'static str,
        requested: u64,
        available: u64,
    },
    Budget {
        domain: &'static str,
        requested: u64,
        available: u64,
    },
    Unsupported {
        at: RecordLocation,
        record: &'static str,
    },
    DepthLimit {
        at: RecordLocation,
        limit: usize,
    },
    InvalidRecord {
        at: RecordLocation,
        record: &'static str,
    },
    Geometry {
        at: RecordLocation,
        reason: String,
    },
    Readback(String),
}
impl GpuError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Busy | Self::Pressure { .. })
    }
}
impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for GpuError {}

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter: wgpu::Adapter,
    pub(super) generation: DeviceGeneration,
    pub(super) limits: GpuLimits,
    lost: Arc<AtomicBool>,
    fault: Arc<Mutex<Option<Arc<wgpu::Error>>>>,
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
        if generation.0 == 0 || limits.max_inflight == 0 || !(1..=64).contains(&limits.max_depth) {
            return Err(GpuError::IdentityExhausted);
        }
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: surface,
                power_preference: wgpu::PowerPreference::None,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| GpuError::Adapter(e.to_string()))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("reel device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| GpuError::Device(e.to_string()))?;
        let lost = Arc::new(AtomicBool::new(false));
        let flag = lost.clone();
        device.set_device_lost_callback(move |_, _| {
            flag.store(true, Ordering::Release);
        });
        let fault = Arc::new(Mutex::new(None));
        let capture = fault.clone();
        device.on_uncaptured_error(Box::new(move |error| {
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
        })
    }
    pub fn check(&self) -> Result<(), GpuError> {
        if self.lost.load(Ordering::Acquire) {
            Err(GpuError::DeviceLost)
        } else if let Some(error) = &*self.fault.lock().expect("device error state poisoned") {
            Err(GpuError::Driver(error.clone()))
        } else {
            Ok(())
        }
    }
    pub fn quality(&self, size: [u32; 2]) -> Result<Quality, GpuError> {
        self.check()?;
        let max = self.device.limits().max_texture_dimension_2d;
        if size.contains(&0) || size.iter().any(|&v| v > max) {
            return Err(GpuError::InvalidSize);
        }
        for (format, samples) in [
            (wgpu::TextureFormat::Rgba16Float, 4),
            (wgpu::TextureFormat::Rgba16Float, 1),
            (wgpu::TextureFormat::Rgba8Unorm, 1),
        ] {
            let features = self.adapter.get_texture_format_features(format);
            let usage =
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
            if !features.allowed_usages.contains(usage)
                || !features
                    .flags
                    .contains(wgpu::TextureFormatFeatureFlags::BLENDABLE)
                || !features.flags.sample_count_supported(samples)
            {
                continue;
            }
            let q = Quality {
                format,
                samples,
                degraded: samples != 4 || format != wgpu::TextureFormat::Rgba16Float,
            };
            if super::targets::target_bytes(size, q)? <= self.limits.target_bytes {
                return Ok(q);
            }
        }
        Err(GpuError::Budget {
            domain: "targets",
            requested: u64::from(size[0]) * u64::from(size[1]) * 4,
            available: self.limits.target_bytes,
        })
    }
}
