//! Native Vita GXM adapter for the shared frame executor.
use reel_ui::{frame::RecordLocation, resources::EffectShaderFormat};
use vita_native::graphics as native;

pub use crate::common::GpuError as GxmError;
use GxmOptions as Options;
pub use limits::Limits as GxmLimits;
pub use renderer::{Backend as GxmBackend, Capture as GxmCapture};
type MemoryUsage = native::MemoryUsage;
mod limits;

/// Native presentation and simultaneous resource limits.
#[derive(Clone, Debug)]
pub struct GxmOptions {
    /// Synchronize presentation with display refresh.
    pub vsync: bool,
    /// Simultaneous GPU and transient resource ceilings.
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

struct MainBudget(reel_core::budget::BudgetPool);
impl vita_native::memory::MemoryBudget for MainBudget {
    fn reserve(
        &self,
        class: vita_native::memory::MemoryClass,
        bytes: u64,
    ) -> Result<
        Box<dyn vita_native::memory::MemoryReservation>,
        vita_native::memory::MemoryBudgetError,
    > {
        if class != vita_native::memory::MemoryClass::MainKernel {
            return Err(vita_native::memory::MemoryBudgetError::Other(
                "unexpected GXM memory class".into(),
            ));
        }
        self.0
            .reserve(bytes)
            .map(|ticket| Box::new(ticket) as Box<dyn vita_native::memory::MemoryReservation>)
            .map_err(|error| match error.code {
                reel_core::result::RejectCode::Busy => {
                    vita_native::memory::MemoryBudgetError::Capacity(error.message)
                }
                _ => vita_native::memory::MemoryBudgetError::Other(error.message),
            })
    }
}

/// Create a native device with Reel's offline clear programs. Hosts that issue
/// native commands directly use the same initialization as the frame executor.
/// An optional MAIN pool accounts rounded native blocks through physical release.
pub fn create_context(
    options: &GxmOptions,
    wake: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    main_budget: Option<reel_core::budget::BudgetPool>,
) -> Result<native::Context, native::Error> {
    let config = native::Config {
        pixel_arena_bytes: options
            .limits
            .total_bytes
            .min(native::Config::default().pixel_arena_bytes),
        main_texture_arena_bytes: options.limits.resource_bytes,
        buffer_arena_bytes: options.limits.transient_bytes,
        upload_staging_bytes: options.limits.transient_bytes.min(4 << 20),
        ..Default::default()
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
        main_budget.map(|pool| {
            std::sync::Arc::new(MainBudget(pool))
                as std::sync::Arc<dyn vita_native::memory::MemoryBudget>
        }),
    )
}

fn create_program(
    context: &native::Context,
    vertex: native::ShaderCode<'_>,
    fragment: native::ShaderCode<'_>,
    bindings: &[native::AttributeBinding<'_>],
    attributes: &[native::VertexAttribute],
    clipped: bool,
) -> Result<native::Program, native::Error> {
    context.create_program(native::ProgramDescriptor {
        vertex,
        fragment,
        bindings,
        stride: 12,
        attributes,
        clip_uniform: clipped.then_some("reel_clip_rect"),
        output: native::FragmentOutput::Patched,
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
