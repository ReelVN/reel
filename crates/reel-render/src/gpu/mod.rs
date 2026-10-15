//! Optional WGPU adapter. Native surface creation belongs to the host assembly.
mod device;
mod targets;
pub use device::{DeviceGeneration, DeviceLoss, Gpu, GpuError, GpuLimits, Quality, SurfaceFailure};
mod mesh;
mod pipeline;
mod renderer;
mod resources;
pub use renderer::{Prepared, Renderer, Usage};
mod glyph;
mod glyph_cache;
mod present;
mod readback;
pub use present::SurfacePass;
mod draw;
mod image_batch;
mod image_cache;
mod replay;
mod scopes;

mod backend;
mod surface;
pub use backend::WgpuBackend;
pub use surface::ContractError as SurfaceContractError;
