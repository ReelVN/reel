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
    #[cfg(all(feature = "wgpu", not(target_arch = "wasm32")))]
    Driver(std::sync::Arc<wgpu::Error>),
    #[cfg(all(feature = "wgpu", target_arch = "wasm32"))]
    Driver(std::sync::Arc<str>),
    InvalidSize,
    IdentityExhausted,
    Busy,
    /// Internal GPU work is continuing preparation of this exact candidate.
    /// Retry the same frame after `ResourcesAvailable`; it has not been presented.
    Preparing,
    /// CPU pixels must be asynchronously restored before this attempt can continue.
    PixelsNeeded(crate::PixelDemand),
    /// Other prepared/in-flight submissions occupy quota needed by this request.
    /// Later records can still exceed the preparation's own quota on retry.
    Pressure {
        domain: &'static str,
        requested: u64,
        available: u64,
    },
    /// The kernel could not allocate a block after native idle reclamation.
    Allocation {
        domain: &'static str,
        requested: u64,
        /// Total free bytes, when available; kernel placement constraints still apply.
        available: Option<u64>,
        committed: u64,
        code: i32,
        /// An outstanding submission may release storage needed by this request.
        pending: bool,
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
    Shader {
        at: RecordLocation,
        asset: String,
        source: Box<GpuError>,
    },
    Readback(String),
}
impl GpuError {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Shader { source, .. } => source.is_retryable(),
            _ => matches!(
                self,
                Self::Busy
                    | Self::Preparing
                    | Self::Pressure { .. }
                    | Self::Allocation { pending: true, .. }
            ),
        }
    }

    #[cfg(any(feature = "gxm", test))]
    pub(crate) fn is_memory_pressure(&self) -> bool {
        match self {
            Self::Shader { source, .. } => source.is_memory_pressure(),
            Self::Pressure { .. } | Self::Allocation { .. } => true,
            _ => false,
        }
    }

    #[cfg(any(feature = "gxm", test))]
    pub(crate) fn after_reclamation(self, pending: bool) -> Self {
        match self {
            Self::Shader { at, asset, source } => Self::Shader {
                at,
                asset,
                source: Box::new(source.after_reclamation(pending)),
            },
            Self::Pressure {
                domain,
                requested,
                available,
            } if !pending => Self::Budget {
                domain,
                requested,
                available,
            },
            Self::Allocation {
                domain,
                requested,
                available,
                committed,
                code,
                ..
            } => Self::Allocation {
                domain,
                requested,
                available,
                committed,
                code,
                pending,
            },
            other => other,
        }
    }
}
impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for GpuError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Shader { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

impl crate::BackendError for GpuError {
    fn into_pixel_demand(self) -> Result<crate::PixelDemand, Self> {
        match self {
            Self::PixelsNeeded(demand) => Ok(demand),
            other => Err(other),
        }
    }
    fn kind(&self) -> crate::FailureKind {
        use crate::FailureKind;
        match self {
            Self::Shader { source, .. } => source.kind(),
            Self::Busy
            | Self::Preparing
            | Self::Pressure { .. }
            | Self::Allocation { pending: true, .. } => FailureKind::Pressure,
            Self::PixelsNeeded(_)
            | Self::Allocation { pending: false, .. }
            | Self::Budget { .. }
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

#[cfg(test)]
mod tests {
    use super::GpuError;
    use crate::{BackendError, FailureKind};
    use reel_ui::core::Widget;

    #[test]
    fn allocation_pressure_waits_only_for_outstanding_owners() {
        let error = GpuError::Allocation {
            domain: "physical CDRAM",
            requested: 16 << 20,
            available: Some(24 << 20),
            committed: 60 << 20,
            code: 0x80024309u32 as i32,
            pending: false,
        };
        assert!(error.is_memory_pressure());
        let waiting = error.after_reclamation(true);
        assert!(waiting.is_retryable());
        assert_eq!(waiting.kind(), FailureKind::Pressure);
        let exhausted = waiting.after_reclamation(false);
        assert!(!exhausted.is_retryable());
        assert_eq!(exhausted.kind(), FailureKind::Refused);
        assert!(matches!(exhausted, GpuError::Allocation {
            requested, available: Some(available), committed, code, pending: false, ..
        } if requested == 16 << 20 && available == 24 << 20
            && committed == 60 << 20 && code == 0x80024309u32 as i32));
    }

    #[test]
    fn shader_context_preserves_memory_failure_and_unrelated_device_errors() {
        let error = GpuError::Shader {
            at: reel_ui::frame::RecordLocation {
                fragment: 0,
                command: 0,
                mask_depth: 0,
                owner: reel_ui::widgets::Label::new("").prepare().id(),
            },
            asset: "effect.gxp".into(),
            source: Box::new(GpuError::Pressure {
                domain: "physical MAIN",
                requested: 4096,
                available: 0,
            }),
        };
        assert!(error.is_memory_pressure());
        let exhausted = error.after_reclamation(false);
        assert_eq!(exhausted.kind(), FailureKind::Refused);
        assert!(matches!(exhausted, GpuError::Shader { source, .. }
            if matches!(*source, GpuError::Budget { requested: 4096, .. })));
        let device = GpuError::Device("invalid native handle".into());
        assert!(!device.is_memory_pressure());
        assert_eq!(device.after_reclamation(true).kind(), FailureKind::Fatal);
    }
}
