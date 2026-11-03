//! Shared CPU geometry, color, glyph rasterization and renderer accounting.
mod error;
#[cfg(feature = "wgpu")]
pub use error::SurfaceFailure;
pub use error::{GpuError, GpuLimits};
pub(crate) mod appearance;
pub(crate) mod backdrop_copy;
pub(crate) mod blend;
pub(crate) mod effect_cache;
pub(crate) mod effect_chain;
pub(crate) mod effect_inputs;
pub(crate) mod effect_output;
pub(crate) mod effect_plan;
pub(crate) mod effect_resolution;
pub(crate) mod effect_result;
pub(crate) mod glyph;
pub(crate) mod glyph_plan;
pub(crate) mod glyph_upload;
pub(crate) mod gradient;
pub(crate) mod image_coverage;
pub(crate) mod image_plan;
pub(crate) mod mask;
pub(crate) mod mesh;
pub(crate) mod renderer;
pub(crate) mod resources;
pub(crate) mod retained;
pub(crate) mod scopes;

pub(crate) mod occlusion_plan;
pub(crate) mod offscreen;
pub(crate) mod precision;
pub(crate) mod quad_instances;
pub(crate) mod sampling_domain;
pub(crate) mod scissor;
pub(crate) mod source_crop;
pub(crate) mod source_plan;
pub(crate) mod source_view;
pub(crate) mod target_cache;
pub(crate) mod target_plan;
pub(crate) mod target_storage;
pub(crate) mod target_writes;
pub(crate) mod usage;

pub(crate) mod draw_batch;
pub(crate) mod geometry;
pub(crate) mod glyph_key;
pub(crate) mod indices;
pub(crate) mod recording_cache;

mod sprites;
