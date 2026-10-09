//! Real device adapter. Window/surface ownership belongs to the assembly crate.
mod device;
mod targets;
pub use device::{DeviceGeneration, Gpu, GpuError, GpuLimits, Quality};
mod mesh;
mod pipeline;
mod renderer;
mod resources;
pub use renderer::{Prepared, Renderer, Usage};
mod glyph;
mod present;
mod readback;
pub use present::SurfacePass;
mod draw;
mod replay;
mod scopes;
