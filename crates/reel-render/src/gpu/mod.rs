//! Optional WGPU adapter. Native surface creation belongs to the host assembly.
mod device;
mod target_cache;
mod targets;
pub use device::{DeviceGeneration, DeviceLoss, Gpu, GpuError, GpuLimits, Quality, SurfaceFailure};
mod mask;
mod mesh;
mod pipeline;
mod renderer;
mod resources;
mod video;
pub use renderer::{Prepared, Renderer, Usage};
mod capture;
mod glyph;
mod glyph_cache;
mod present;
mod readback;
pub use capture::GpuCapture;
pub use present::SurfacePass;
mod draw;
mod image_batch;
mod image_cache;
mod image_plan;
mod replay;
mod scopes;

mod backend;
mod surface;
pub use backend::WgpuBackend;
pub use surface::ContractError as SurfaceContractError;

#[cfg(test)]
mod blend_tests;
