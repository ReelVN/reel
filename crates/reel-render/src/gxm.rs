//! Native Vita GXM adapter for the shared frame executor.
use reel_ui::{frame::RecordLocation, resources::EffectShaderFormat};
use vita_native::graphics as native;

pub use crate::common::GpuError as GxmError;
use GxmOptions as Options;
pub use limits::Limits as GxmLimits;
pub use renderer::{Backend as GxmBackend, Capture as GxmCapture};
type MemoryUsage = native::MemoryUsage;
mod limits;

fn destination_access() -> crate::common::blend::DestinationAccess {
    if cfg!(feature = "gxm-tile-destination") {
        crate::common::blend::DestinationAccess::CurrentPixelUnorm8
    } else {
        crate::common::blend::DestinationAccess::Sampled
    }
}

/// Native presentation and work concurrency.
#[derive(Clone, Debug)]
pub struct GxmOptions {
    /// Synchronize presentation with display refresh.
    pub vsync: bool,
    /// In-flight and upload work limits; storage follows native physical capacity.
    pub limits: GxmLimits,
}
impl Default for GxmOptions {
    fn default() -> Self {
        Self {
            vsync: true,
            limits: Default::default(),
        }
    }
}

macro_rules! shader_code {
    ($name:literal) => {
        native::ShaderCode::GxpBinary(include_bytes!(concat!(env!("OUT_DIR"), "/", $name, ".gxp")))
    };
}
mod renderer;

/// Create a native device with Reel's offline clear programs. Hosts that issue
/// native commands directly use the same initialization as the frame executor.
/// Native physical admission accounts rounded blocks through physical release.
/// Shared scheduling targets are provisioned for eight scenes per frame; driver
/// storage is included in each target footprint before allocation.
/// Screen draws use `native::FragmentOutput::Srgb` with linear input colors;
/// offscreen targets retain linear color encoding and use 32x32 tiled RGBA8
/// storage when both dimensions are at least 32. Small and Coverage8 targets
/// remain scanline-addressed; native layout queries account complete tiles.
pub fn create_context(
    options: &GxmOptions,
    wake: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
) -> Result<native::Context, native::Error> {
    let config = native::Config {
        display_encoding: native::SurfaceEncoding::Srgb,
        // Several independent color surfaces share one scheduling target. GXM
        // blocks the CPU when that target's scene records are exhausted (libgxm
        // Overview, Render Targets); use the supported queue depth for effect
        // chains rather than the native adapter's general-purpose default.
        scenes_per_frame: 8,
        target_storage: native::TargetStorage::Tiled,
        ..native::Config::default()
    };
    native::Context::with_config(
        config,
        options.vsync,
        native::ClearProgram {
            vertex: shader_code!("clear_vertex"),
            fragment: shader_code!("clear"),
            position_attribute: "a_position",
            color_uniform: "u_clear_color",
            clip_uniform: "reel_clip_rect",
        },
        wake,
        None,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "one native pipeline and its reflected vertex interface"
)]
fn create_program(
    context: &native::Context,
    vertex: native::ShaderCode<'_>,
    fragment: native::ShaderCode<'_>,
    bindings: &[native::AttributeBinding<'_>],
    layout: native::VertexLayout<'_>,
    instance: Option<native::VertexLayout<'_>>,
    clipped: bool,
    output: native::FragmentOutput,
) -> Result<native::Program, native::Error> {
    context.create_program(native::ProgramDescriptor {
        vertex,
        fragment,
        bindings,
        stride: layout.stride,
        attributes: layout.attributes,
        instance,
        clip_uniform: clipped.then_some("reel_clip_rect"),
        output,
    })
}

fn create_target_texture(
    context: &native::Context,
    size: [u32; 2],
    format: native::TargetFormat,
) -> Result<native::Texture, native::Error> {
    context.create_target_texture(size, format)
}

fn texture_bytes(size: [u32; 2], format: native::TextureFormat) -> Result<u64, GxmError> {
    Ok(native::texture_layout(size, format)?.bytes as u64)
}

fn effect_code(
    format: EffectShaderFormat,
    bytes: &[u8],
    at: RecordLocation,
) -> Result<native::ShaderCode<'_>, GxmError> {
    if format != EffectShaderFormat::Gxp {
        return Err(GxmError::Unsupported {
            at,
            record: "GXM requires an exported GXP effect shader",
        });
    }
    Ok(native::ShaderCode::GxpBinary(bytes))
}

fn native_error(error: native::Error) -> GxmError {
    match error {
        native::Error::Allocation {
            domain,
            requested,
            available,
            committed,
            code,
        } => GxmError::Allocation {
            domain,
            requested,
            available,
            committed,
            code,
            pending: false,
        },
        native::Error::Budget {
            domain: domain @ ("physical MAIN" | "physical CDRAM"),
            requested,
            limit,
        } => GxmError::Pressure {
            domain,
            requested,
            available: limit,
        },
        native::Error::Budget {
            domain,
            requested,
            limit,
        } => GxmError::Budget {
            domain,
            requested,
            available: limit,
        },
        native::Error::Terminal(_) => GxmError::NativeDeviceLost(error.to_string()),
        _ => GxmError::Device(error.to_string()),
    }
}
impl From<native::Error> for GxmError {
    fn from(error: native::Error) -> Self {
        native_error(error)
    }
}
