//! Optional WGPU adapter. Native surface creation belongs to the host assembly.
mod device;
mod target_cache;
mod targets;
pub use device::{DeviceGeneration, DeviceLoss, Gpu, GpuError, GpuLimits, Quality, SurfaceFailure};
mod effect;
mod effect_uniform;
mod mask;
pub(crate) use crate::common::mesh;
mod pipeline;
mod renderer;
mod resources;
mod retained;
mod video;
pub use crate::Usage;
pub use renderer::{Prepared, Renderer};
mod capture;
pub(crate) use crate::common::glyph;
mod glyph_cache;
pub(crate) use crate::common::gradient;
mod present;
#[cfg(not(target_arch = "wasm32"))]
mod readback;
pub use capture::GpuCapture;
pub use present::SurfacePass;
mod draw;
mod geometry;
mod image_batch;
mod image_cache;
mod image_plan;
mod replay;
mod upload;
pub(crate) use crate::common::scopes;

mod backend;
mod surface;
pub use backend::WgpuBackend;
pub use surface::ContractError as SurfaceContractError;

#[cfg(test)]
mod blend_tests;
