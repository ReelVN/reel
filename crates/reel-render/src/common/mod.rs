//! Shared CPU geometry, color, glyph rasterization and renderer accounting.
mod error;
#[cfg(feature = "wgpu")]
pub use error::SurfaceFailure;
pub use error::{GpuError, GpuLimits};
pub(crate) mod appearance;
pub(crate) mod effect_cache;
pub(crate) mod effect_plan;
pub(crate) mod glyph;
pub(crate) mod glyph_plan;
pub(crate) mod gradient;
pub(crate) mod image_plan;
pub(crate) mod image_coverage;
pub(crate) mod mask;
pub(crate) mod mesh;
pub(crate) mod renderer;
pub(crate) mod resources;
pub(crate) mod scopes;

pub(crate) mod offscreen;
pub(crate) mod scissor;
pub(crate) mod source_view;
pub(crate) mod target_plan;
pub(crate) mod target_writes;

pub(crate) mod draw_batch;
pub(crate) mod geometry;
pub(crate) mod indices;
pub(crate) mod glyph_key;
