use reel_ui::frame::RecordLocation;
#[derive(Clone, Copy, Debug)]
pub struct GpuLimits {
    pub target_bytes: u64,
    pub resource_bytes: u64,
    pub transient_bytes: u64,
    pub max_inflight: usize,
    /// Maximum converted image staging bytes per preparation. A single complete
    /// source row may exceed this limit so every valid image can make progress.
    pub image_upload_bytes_per_prepare: u64,
}
impl Default for GpuLimits {
    fn default() -> Self {
        Self {
            target_bytes: 128 << 20,
            resource_bytes: 128 << 20,
            transient_bytes: 32 << 20,
            max_inflight: 2,
            image_upload_bytes_per_prepare: 4 << 20,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(feature = "wgpu")]
pub enum SurfaceFailure {
    Lost,
    Outdated,
    Validation,
}
#[derive(Debug)]
pub enum GpuError {
    #[cfg(feature = "wgpu")]
    Surface(SurfaceFailure),
    #[cfg(feature = "wgpu")]
    SurfaceContract(crate::gpu::SurfaceContractError),
    InvalidSurface,
    WrongSurface,
    Adapter(String),
    Device(String),
    NativeSurface(String),
    NativeDeviceLost(String),
    #[cfg(feature = "wgpu")]
    DeviceLost(std::sync::Arc<crate::gpu::DeviceLoss>),
    WrongDevice,
    #[cfg(feature = "wgpu")]
    Driver(std::sync::Arc<wgpu::Error>),
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

impl crate::BackendError for GpuError {
    fn kind(&self) -> crate::FailureKind {
        use crate::FailureKind;
        match self {
            Self::Busy | Self::Pressure { .. } => FailureKind::Pressure,
            Self::Budget { .. }
            | Self::Unsupported { .. }
            | Self::InvalidRecord { .. }
            | Self::Geometry { .. }
            | Self::InvalidSize
            | Self::InvalidSurface
            | Self::WrongSurface
            | Self::WrongDevice => FailureKind::Refused,
            Self::NativeSurface(_) => FailureKind::SurfaceLost,
            Self::NativeDeviceLost(_) => FailureKind::DeviceLost,
            #[cfg(feature = "wgpu")]
            Self::Surface(SurfaceFailure::Lost | SurfaceFailure::Outdated) => {
                FailureKind::SurfaceLost
            }
            #[cfg(feature = "wgpu")]
            Self::DeviceLost(_) => FailureKind::DeviceLost,
            _ => FailureKind::Fatal,
        }
    }
}
