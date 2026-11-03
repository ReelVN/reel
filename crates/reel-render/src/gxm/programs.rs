use super::*;
use native::{
    AttributeBinding, Blend, Draw, Framebuffer, IndexBuffer, Program, ShaderCode,
    Target as NativeTarget, Texture, Uniform, UniformValue, VertexAttribute, VertexBuffer,
};
use smallvec::SmallVec;
pub(super) struct Quad<'a> {
    pub target: NativeTarget<'a>,
    pub size: [u32; 2],
    pub shader: &'a Shader,
    pub textures: &'a [&'a Texture],
    pub values: &'a [UniformValue<'a>],
    pub blend: Blend,
    pub scissor: Option<[u32; 4]>,
}

const NAMES: &[&str] = &[
    "u_image",
    "u_chroma_u",
    "u_chroma_v",
    "u_dimensions",
    "u_output_size",
    "u_repeat",
    "u_appearance[0]",
    "u_video[0]",
    "u_video_kind",
    "u_image_flip",
    "u_coverage",
    "u_source",
    "u_source_uv",
    "u_mask_uv",
    "u_backdrop_uv",
    "u_expression_uv",
    "u_mask",
    "u_backdrop",
    "u_expression",
    "u_params",
    "u_invert",
    "u_blend",
    "u_has_expression",
    "u_ops[0]",
    "u_texels[0]",
    "u_dimensions[0]",
    "u_uv0",
    "u_uv1",
    "u_values",
    "u_flags",
    "u_count",
    "u_image0",
    "u_image1",
    "u_image2",
    "u_image3",
    "u_texels",
    "u_coordinates",
    "u_sample_flags",
    "u_step0",
    "u_step1",
    "u_step2",
    "u_step3",
    "u_step4",
    "u_step5",
    "u_step6",
    "u_step7",
    "u_coordinates[0]",
    "u_fetches[0]",
    "u_seed0",
    "u_seed1",
    "u_origin",
    "u_origins[0]",
    "u_quad_rect",
    "u_quad_scale",
];
pub(super) type Values<'a> = SmallVec<[UniformValue<'a>; 24]>;
#[path = "programs/paint.rs"]
mod paint;
pub(super) use paint::Kind as PaintKind;
pub(super) struct Shader {
    pub program: Program,
    uniforms: [Option<Uniform>; NAMES.len()],
}
impl Shader {
    fn composite(
        context: &super::ownership::Device,
        fragment: ShaderCode<'_>,
    ) -> Result<Self, GpuError> {
        Self::with_clipping(context, shader_code!("composite_vertex"), fragment, false)
    }
    fn new(context: &super::ownership::Device, fragment: ShaderCode<'_>) -> Result<Self, GpuError> {
        Self::with_vertex(context, shader_code!("vertex"), fragment)
    }
    fn with_vertex(
        context: &super::ownership::Device,
        vertex: ShaderCode<'_>,
        fragment: ShaderCode<'_>,
    ) -> Result<Self, GpuError> {
        Self::with_clipping(context, vertex, fragment, true)
    }
    fn with_clipping(
        context: &super::ownership::Device,
        vertex: ShaderCode<'_>,
        fragment: ShaderCode<'_>,
        clipped: bool,
    ) -> Result<Self, GpuError> {
        let program = create_program_with_vertex(context, vertex, fragment, clipped, false)?;
        let uniforms = std::array::from_fn(|index| program.uniform(NAMES[index]));
        Ok(Self { program, uniforms })
    }
    fn presentation(context: &super::ownership::Device) -> Result<Self, GpuError> {
        let program = create_program_with_output(
            context,
            shader_code!("vertex"),
            shader_code!("present"),
            false,
            false,
            native::FragmentOutput::Srgb,
        )?;
        let uniforms = std::array::from_fn(|index| program.uniform(NAMES[index]));
        Ok(Self { program, uniforms })
    }
    fn uniform(&self, name: &str) -> Option<&Uniform> {
        self.uniforms[NAMES
            .iter()
            .position(|&candidate| candidate == name)
            .expect("known uniform")]
        .as_ref()
    }
    pub fn float<'a>(&'a self, name: &str, value: f32) -> Option<UniformValue<'a>> {
        self.uniform(name).map(|u| UniformValue::Float(u, value))
    }
    pub fn int<'a>(&'a self, name: &str, value: i32) -> Option<UniformValue<'a>> {
        self.uniform(name).map(|u| UniformValue::Int(u, value))
    }
    pub fn vec2<'a>(&'a self, name: &str, value: [f32; 2]) -> Option<UniformValue<'a>> {
        self.uniform(name).map(|u| UniformValue::Vec2(u, value))
    }
    pub fn vec4<'a>(&'a self, name: &str, value: [f32; 4]) -> Option<UniformValue<'a>> {
        self.uniform(name).map(|u| UniformValue::Vec4(u, value))
    }
    pub fn array<'a>(&'a self, name: &str, value: &'a [[f32; 4]]) -> Option<UniformValue<'a>> {
        self.uniform(name)
            .map(|u| UniformValue::Vec4Array(u, value))
    }
}
pub(super) fn create_program(
    context: &super::ownership::Device,
    fragment: ShaderCode<'_>,
    implicit_source: bool,
    padded: bool,
    clipped: bool,
) -> Result<Program, GpuError> {
    create_program_with_vertex(
        context,
        if padded {
            shader_code!("effect_padded_vertex")
        } else if implicit_source {
            shader_code!("effect_vertex")
        } else {
            shader_code!("vertex")
        },
        fragment,
        clipped,
        false,
    )
}
fn create_program_with_vertex(
    context: &super::ownership::Device,
    vertex: ShaderCode<'_>,
    fragment: ShaderCode<'_>,
    clipped: bool,
    instanced: bool,
) -> Result<Program, GpuError> {
    create_program_with_output(
        context,
        vertex,
        fragment,
        clipped,
        instanced,
        native::FragmentOutput::Patched,
    )
}

fn create_program_with_output(
    context: &super::ownership::Device,
    vertex: ShaderCode<'_>,
    fragment: ShaderCode<'_>,
    clipped: bool,
    instanced: bool,
    output: native::FragmentOutput,
) -> Result<Program, GpuError> {
    let vertex_attributes = super::geometry::vertex_attributes();
    let instance_attributes = super::geometry::instance_attributes();
    let regular_attributes = attributes();
    let program = super::super::create_program(
        context,
        vertex,
        fragment,
        &[
            AttributeBinding {
                location: 0,
                name: if instanced { "a_corner" } else { "a_position" },
            },
            AttributeBinding {
                location: 1,
                name: "a_uv",
            },
            AttributeBinding {
                location: 2,
                name: "a_color",
            },
            AttributeBinding {
                location: 3,
                name: "a_window",
            },
            AttributeBinding {
                location: 4,
                name: "a_positions01",
            },
            AttributeBinding {
                location: 5,
                name: "a_positions23",
            },
        ],
        native::VertexLayout {
            stride: if instanced { 7 } else { 12 },
            attributes: if instanced {
                &vertex_attributes
            } else {
                &regular_attributes
            },
        },
        instanced.then_some(native::VertexLayout {
            stride: 12,
            attributes: &instance_attributes,
        }),
        clipped,
        output,
    )?;
    Ok(program)
}
pub(super) struct Programs {
    pub paint: paint::PaintPrograms,
    pub composite: Shader,
    pub extended_blend: Shader,
    transition: [std::cell::OnceCell<Shader>; 2],
    #[cfg(feature = "gxm-tile-destination")]
    tile: [std::cell::OnceCell<Shader>; 13],
    pub source_over: Shader,
    pub effect_resample: Shader,
    pub alpha_mask: Shader,
    pub mask_8: Shader,
    pub mask_16: Shader,
    pub mask_16_compact: Shader,
    pub mask_split_1: Shader,
    pub mask_split_2: Shader,
    pub mask_split_2_alpha: Shader,
    pub mask_32_compact: Shader,
    pub mask: Shader,
    pub mask_sample_chain: Shader,
    pub mask_threshold: Shader,
    pub mask_sample_chain_composite: Shader,
    pub mask_threshold_composite: Shader,
    pub mask_paired: [Shader; 4],
    pub mask_paired_composite: [Shader; 4],
    pub present: Shader,
    pub vertices: VertexBuffer,
    pub indices: IndexBuffer,
    _storage: super::ownership::NativeTicket,
}
impl Programs {
    pub fn new(context: &super::ownership::Device, budget: &BudgetPool) -> Result<Self, GpuError> {
        let bytes =
            (size_of::<Self>() + 4 * size_of::<crate::common::mesh::Vertex>() + 6 * 2) as u64;
        let storage = budget
            .reserve(bytes)
            .map_err(|e| GpuError::Device(e.message))?;
        let storage = context.ticket(storage);
        let attributes = attributes();
        let data = [
            [-1., 1., 0., 0.],
            [1., 1., 1., 0.],
            [1., -1., 1., 1.],
            [-1., -1., 0., 1.],
        ]
        .map(|p| crate::common::mesh::Vertex {
            position: [p[0], p[1]],
            uv: [p[2], p[3]],
            color: [1.; 4],
            source_window: [0.; 4],
        });
        Ok(Self {
            transition: std::array::from_fn(|_| std::cell::OnceCell::new()),
            #[cfg(feature = "gxm-tile-destination")]
            tile: std::array::from_fn(|_| std::cell::OnceCell::new()),
            paint: paint::PaintPrograms::new(budget),
            composite: Shader::composite(context, shader_code!("composite"))?,
            extended_blend: Shader::composite(context, shader_code!("extended_blend"))?,
            source_over: Shader::composite(context, shader_code!("source_over"))?,
            effect_resample: Shader::composite(context, shader_code!("effect_resample"))?,
            alpha_mask: Shader::composite(context, shader_code!("alpha_mask"))?,
            mask_8: Shader::new(context, shader_code!("mask_8"))?,
            mask_16: Shader::new(context, shader_code!("mask_16"))?,
            mask_16_compact: Shader::new(context, shader_code!("mask_16_compact"))?,
            mask_split_1: Shader::new(context, shader_code!("mask_split_1"))?,
            mask_split_2: Shader::new(context, shader_code!("mask_split_2"))?,
            mask_split_2_alpha: Shader::new(context, shader_code!("mask_split_2_alpha"))?,
            mask_32_compact: Shader::new(context, shader_code!("mask_32_compact"))?,
            mask: Shader::new(context, shader_code!("mask"))?,
            mask_sample_chain: Shader::new(context, shader_code!("mask_sample_chain"))?,
            mask_threshold: Shader::new(context, shader_code!("mask_threshold"))?,
            // Composite quads already end at the integer clip bounds. Keeping
            // discard here would unnecessarily select a feedback pixel program.
            mask_sample_chain_composite: Shader::composite(
                context,
                shader_code!("mask_sample_chain_composite"),
            )?,
            mask_threshold_composite: Shader::composite(
                context,
                shader_code!("mask_threshold_composite"),
            )?,
            mask_paired: [
                Shader::new(context, shader_code!("mask_paired_aa"))?,
                Shader::new(context, shader_code!("mask_paired_ag"))?,
                Shader::new(context, shader_code!("mask_paired_ga"))?,
                Shader::new(context, shader_code!("mask_paired_gg"))?,
            ],
            mask_paired_composite: [
                Shader::composite(context, shader_code!("mask_paired_aa_composite"))?,
                Shader::composite(context, shader_code!("mask_paired_ag_composite"))?,
                Shader::composite(context, shader_code!("mask_paired_ga_composite"))?,
                Shader::composite(context, shader_code!("mask_paired_gg_composite"))?,
            ],
            // The final quad covers the viewport with no narrower scissor.
            // Omitting discard lets GXM schedule the texture fetch independently.
            present: Shader::presentation(context)?,
            vertices: context.vertex_buffer(bytemuck::cast_slice(&data), 12, &attributes)?,
            indices: context.index_buffer_u16(&[0, 1, 2, 0, 2, 3])?,
            _storage: storage,
        })
    }
    #[cfg(feature = "gxm-tile-destination")]
    pub fn tile(
        &self,
        context: &super::ownership::Device,
        blend: u32,
        invert: f32,
    ) -> Result<&Shader, GpuError> {
        let specialized = invert == 0. && matches!(blend, 1..=3 | 5..=10);
        let slot = &self.tile[if specialized {
            blend as usize + 1
        } else {
            usize::from(blend > 4)
        }];
        if slot.get().is_none() {
            let fragment = if specialized {
                match blend {
                    1 => shader_code!("tile_blend_1"),
                    2 => shader_code!("tile_blend_2"),
                    3 => shader_code!("tile_blend_3"),
                    5 => shader_code!("tile_blend_5"),
                    6 => shader_code!("tile_blend_6"),
                    7 => shader_code!("tile_blend_7"),
                    8 => shader_code!("tile_blend_8"),
                    9 => shader_code!("tile_blend_9"),
                    10 => shader_code!("tile_blend_10"),
                    _ => unreachable!("specialized blend family"),
                }
            } else if blend > 4 {
                shader_code!("tile_extended_blend")
            } else {
                shader_code!("tile_composite")
            };
            let program = create_program_with_output(
                context,
                shader_code!("composite_vertex"),
                fragment,
                false,
                false,
                native::FragmentOutput::Declared,
            )?;
            let uniforms = std::array::from_fn(|index| program.uniform(NAMES[index]));
            assert!(
                slot.set(Shader { program, uniforms }).is_ok(),
                "single-threaded program creation"
            );
        }
        Ok(slot.get().unwrap())
    }

    pub fn transition(
        &self,
        context: &super::ownership::Device,
        masked: bool,
    ) -> Result<&Shader, GpuError> {
        let slot = &self.transition[usize::from(masked)];
        if slot.get().is_none() {
            let shader = Shader::composite(
                context,
                if masked {
                    shader_code!("masked_transition")
                } else {
                    shader_code!("transition")
                },
            )?;
            assert!(slot.set(shader).is_ok(), "single-threaded program creation");
        }
        Ok(slot.get().unwrap())
    }
    pub fn quad(&self, context: &super::ownership::Device, quad: Quad<'_>) -> Result<(), GpuError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Draw);
        let Quad {
            target,
            size,
            shader,
            textures,
            values,
            blend,
            scissor,
        } = quad;
        let mut geometry_values = Values::new();
        let mut draw_size = size;
        let mut draw_scissor = scissor;
        let values = if let Some(uniform) = shader.uniform("u_quad_rect") {
            let [x, y, width, height] = scissor.unwrap_or([0, 0, size[0], size[1]]);
            // The cropped quad owns pixel clipping. Use the physical viewport
            // so pooled padding does not request a second fragment clip.
            draw_size = match target {
                NativeTarget::Framebuffer(framebuffer) => framebuffer.size(),
                NativeTarget::Screen => size,
            };
            draw_scissor = None;
            geometry_values.extend_from_slice(values);
            geometry_values.push(UniformValue::Vec4(
                uniform,
                [x as f32, y as f32, width as f32, height as f32],
            ));
            geometry_values.extend(shader.vec4(
                "u_quad_scale",
                [
                    2. / draw_size[0] as f32,
                    -2. / draw_size[1] as f32,
                    1. / size[0] as f32,
                    1. / size[1] as f32,
                ],
            ));
            geometry_values.as_slice()
        } else {
            values
        };
        context.draw(Draw {
            target,
            size: draw_size,
            program: &shader.program,
            vertices: &self.vertices,
            instances: None,
            indices: Some(&self.indices),
            range: 0..6,
            textures,
            uniforms: values,
            blend,
            scissor: draw_scissor,
        })?;
        Ok(())
    }
    pub fn present(
        &self,
        context: &super::ownership::Device,
        target: &resources::Target,
        size: [u32; 2],
    ) -> Result<(), GpuError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Screen);
        // This pass replaces the whole surface. Match WGPU's clear load op so
        // tile renderers need not restore the previous swapchain image first.
        {
            let _clear = reel_core::profile::Scope::new(reel_core::profile::Phase::Clear);
            context.clear(NativeTarget::Screen, [0., 0., 0., 1.])?;
        }
        let mut values = Values::new();
        values.extend(self.present.int("u_image", 0));
        self.quad(
            context,
            programs::Quad {
                target: NativeTarget::Screen,
                size,
                shader: &self.present,
                textures: &[&target.view],
                values: &values,
                blend: Blend::Replace,
                scissor: None,
            },
        )
    }
}
pub(super) fn attributes() -> [VertexAttribute; 4] {
    [
        VertexAttribute {
            location: 0,
            components: 2,
            offset: 0,
        },
        VertexAttribute {
            location: 1,
            components: 2,
            offset: 2,
        },
        VertexAttribute {
            location: 2,
            components: 4,
            offset: 4,
        },
        VertexAttribute {
            location: 3,
            components: 4,
            offset: 8,
        },
    ]
}
struct TargetStorage {
    // Drop native cell owners before releasing their one shared budget ticket.
    macrotile: Option<native::MacrotileTarget>,
    _ticket: super::ownership::NativeTicket,
    effect_pins: std::cell::Cell<usize>,
    #[cfg(feature = "profiling")]
    trace_id: u64,
}
#[cfg(feature = "profiling")]
impl Drop for TargetStorage {
    fn drop(&mut self) {
        crate::profiling::record_trace(
            crate::profiling::TraceKind::TargetCache,
            crate::profiling::TraceData {
                label: "owner-drop-begin",
                status: crate::profiling::TraceStatus::Ok,
                output_target: self.trace_id,
                bytes: self._ticket.bytes(),
                count: 1,
                ..Default::default()
            },
        );
    }
}
#[cfg(feature = "profiling")]
fn trace_native_code(error: &native::Error) -> Option<i32> {
    match error {
        native::Error::Sdk(_, code) | native::Error::Allocation { code, .. } => Some(*code),
        native::Error::Terminal(fault) => Some(fault.code),
        _ => None,
    }
}
#[cfg(feature = "profiling")]
fn target_request_trace(
    size: [u32; 2],
    format: native::TargetFormat,
    storage: native::TargetStorage,
) -> crate::profiling::TraceData {
    use crate::profiling::*;
    TraceData {
        storage: size,
        output_extent: size,
        count: 1,
        target_format: match format {
            native::TargetFormat::Rgba8 => 1,
            native::TargetFormat::Coverage8 => 2,
        },
        storage_kind: match storage {
            native::TargetStorage::Linear => 1,
            native::TargetStorage::Tiled => 2,
        },
        known: KNOWN_STORAGE | KNOWN_OUTPUT_EXTENT | KNOWN_TARGET_FORMAT | KNOWN_STORAGE_KIND,
        ..Default::default()
    }
}
struct EffectPin {
    storage: Rc<TargetStorage>,
    _metadata: BudgetTicket,
}
impl Drop for EffectPin {
    fn drop(&mut self) {
        self.storage
            .effect_pins
            .set(self.storage.effect_pins.get() - 1);
        #[cfg(feature = "profiling")]
        crate::profiling::record_trace(
            crate::profiling::TraceKind::TargetCache,
            crate::profiling::TraceData {
                label: "effect-unpin",
                status: crate::profiling::TraceStatus::Ok,
                output_target: self.storage.trace_id,
                bytes: self.storage._ticket.bytes(),
                count: self.storage.effect_pins.get() as u64,
                ..Default::default()
            },
        );
    }
}
#[derive(Clone)]
pub(super) struct Target {
    pub effect_cache_allowed: bool,
    pub slot: usize,
    pub size: [u32; 2],
    pub source_crop: Option<crate::common::source_crop::Crop>,
    pub view: Texture,
    pub framebuffer: Framebuffer,
    _storage: Rc<TargetStorage>,
    effect_content: Option<crate::common::effect_result::Token>,
    effect_pin: Option<Rc<EffectPin>>,
    #[cfg(feature = "profiling")]
    trace_view: u64,
    #[cfg(feature = "profiling")]
    trace_lease: u64,
    #[cfg(feature = "profiling")]
    trace_region: Option<[u32; 4]>,
    #[cfg(feature = "profiling")]
    trace_cell: Option<u32>,
}
impl Target {
    #[cfg(feature = "profiling")]
    pub(super) fn trace_id(&self) -> u64 {
        self._storage.trace_id
    }
    #[cfg(feature = "profiling")]
    pub(super) fn trace_view_id(&self) -> u64 {
        self.trace_view
    }
    #[cfg(feature = "profiling")]
    pub(super) fn trace_lease_id(&self) -> u64 {
        self.trace_lease
    }
    #[cfg(feature = "profiling")]
    /// A macro cell's write window is atlas-absolute; ordinary windows are view-local.
    /// The mirror observes admitted bounds, not shaded pixels or PBE traffic.
    pub(super) fn trace_data(&self) -> crate::profiling::TraceData {
        use crate::profiling::*;
        let view = self.view.size();
        let mut data = TraceData {
            output_target: self.trace_id(),
            view_id: self.trace_view,
            lease_id: self.trace_lease,
            logical: self.size,
            storage: self.macrotile_extent().unwrap_or(view),
            output_extent: view,
            bytes: self.view.allocated_bytes() as u64,
            stride_bytes: self.view.layout().row_stride_bytes as u64,
            known: KNOWN_LOGICAL | KNOWN_STORAGE | KNOWN_OUTPUT_EXTENT | KNOWN_STRIDE,
            ..Default::default()
        };
        if let Some(storage) = self.view.target_storage() {
            data.storage_kind = match storage {
                native::TargetStorage::Linear => 1,
                native::TargetStorage::Tiled => 2,
            };
            data.known |= KNOWN_STORAGE_KIND;
        }
        data.target_format = match self.view.format() {
            native::TextureFormat::Rgba8 => 1,
            native::TextureFormat::Coverage8 => 2,
            _ => 0,
        };
        if data.target_format != 0 {
            data.known |= KNOWN_TARGET_FORMAT;
        }
        data.write_region = self.trace_region.unwrap_or_else(|| {
            self._storage
                .macrotile
                .as_ref()
                .zip(self.trace_cell)
                .and_then(|(owner, cell)| owner.layout().cell_rect(cell))
                .unwrap_or([0, 0, view[0], view[1]])
        });
        data.valid_region = self.macrotile_extent().unwrap_or([
            data.write_region[0] + data.write_region[2],
            data.write_region[1] + data.write_region[3],
        ]);
        data.known |= KNOWN_WRITE_REGION | KNOWN_VALID_REGION;
        data
    }
    #[cfg(feature = "profiling")]
    pub(super) fn trace_event(&self, kind: crate::profiling::TraceKind, label: &'static str) {
        if !crate::profiling::trace_enabled() {
            return;
        }
        let mut data = self.trace_data();
        data.label = label;
        data.status = crate::profiling::TraceStatus::Ok;
        data.count = 1;
        crate::profiling::record_trace(kind, data);
    }
    #[cfg(feature = "profiling")]
    pub(super) fn trace_write_region(&mut self, region: Option<[u32; 4]>) {
        self.trace_region = region;
    }
    #[cfg(feature = "profiling")]
    pub(super) fn trace_begin_use(&mut self) {
        self.trace_lease = crate::profiling::targets::next_id();
        self.trace_event(crate::profiling::TraceKind::TargetAcquire, "lease-begin");
    }
    #[cfg(feature = "profiling")]
    pub(super) fn trace_share_use(&mut self, source: &Self) {
        self.trace_lease = source.trace_lease;
        self.trace_region = source.trace_region;
    }
    pub(super) fn macrotile_extent(&self) -> Option<[u32; 2]> {
        self._storage
            .macrotile
            .as_ref()
            .map(|owner| owner.layout().size())
    }
    pub(super) fn cache_only_owners(&self, owners: usize) -> bool {
        Rc::strong_count(&self._storage) == owners
    }
    pub(super) fn is_effect_pinned(&self) -> bool {
        self._storage.effect_pins.get() != 0
    }
    pub(super) fn effect_token(&self) -> Option<crate::common::effect_result::Token> {
        self.effect_content
    }
    /// Label the completed pixels of this logical view without retaining them.
    pub(super) fn with_effect_content(
        mut self,
        token: crate::common::effect_result::Token,
    ) -> Self {
        self.effect_content = Some(token);
        self
    }
    /// End the old logical content before this scratch view can be written again.
    pub(super) fn clear_effect_content(&mut self) {
        self.effect_content = None;
    }
    pub(super) fn pin_effect(
        &self,
        token: crate::common::effect_result::Token,
        pool: &BudgetPool,
    ) -> Option<Self> {
        let metadata = pool
            .reserve((size_of::<EffectPin>() + 2 * size_of::<usize>()) as u64)
            .ok()?;
        self._storage
            .effect_pins
            .set(self._storage.effect_pins.get().checked_add(1)?);
        let mut result = self.clone().with_effect_content(token);
        result.effect_pin = Some(Rc::new(EffectPin {
            storage: self._storage.clone(),
            _metadata: metadata,
        }));
        #[cfg(feature = "profiling")]
        result.trace_event(crate::profiling::TraceKind::TargetCache, "effect-pin");
        Some(result)
    }
    pub fn source_uv(&self, parent: [u32; 2], origin: [i32; 2]) -> [f32; 4] {
        let storage = self.view.size();
        crate::common::source_view::uv_mapping(parent, storage, origin)
    }
    pub fn frame_uv(&self, parent: [u32; 2]) -> [f32; 4] {
        crate::common::source_view::uv_mapping(parent, self.view.size(), [0; 2])
    }
    pub fn same_allocation(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self._storage, &other._storage)
    }
    pub(super) fn cache_only(&self) -> bool {
        Rc::strong_count(&self._storage) == 1
    }
    /// Replace only the immutable native descriptors of one linear allocation.
    /// The exact requested raster and the complete physical budget owner survive.
    pub(super) fn reshaped(
        &self,
        context: &super::ownership::Device,
        size: [u32; 2],
    ) -> Result<Self, GpuError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::TargetReshape);
        #[cfg(feature = "profiling")]
        let mut trace = {
            let mut data = self.trace_data();
            data.source_target = data.output_target;
            data.source_view_id = data.view_id;
            data.source_lease_id = data.lease_id;
            data.source_extent = self.view.size();
            data.logical = size;
            data.storage = size;
            data.output_extent = size;
            // The new immutable descriptor does not exist until construction
            // succeeds; its requested geometry has no observed write window yet.
            data.known &= !(crate::profiling::KNOWN_WRITE_REGION
                | crate::profiling::KNOWN_VALID_REGION
                | crate::profiling::KNOWN_STRIDE);
            data.write_region = [0; 4];
            data.valid_region = [0; 2];
            data.stride_bytes = 0;
            data.known |= crate::profiling::KNOWN_SOURCE_EXTENT;
            crate::profiling::targets::Operation::new("reshape", data)
        };
        #[cfg(feature = "profiling")]
        trace.stage("reshape-texture");
        let view = context
            .reshape_target_texture(&self.view, size)
            .map_err(|error| {
                #[cfg(feature = "profiling")]
                trace.fail(trace_native_code(&error));
                error
            })?;
        #[cfg(feature = "profiling")]
        trace.stage("reshape-framebuffer");
        let framebuffer = context.framebuffer(&view).map_err(|error| {
            #[cfg(feature = "profiling")]
            trace.fail(trace_native_code(&error));
            error
        })?;
        let mut result = self.clone();
        result.view = view;
        result.framebuffer = framebuffer;
        result.size = size;
        result.source_crop = None;
        result.clear_effect_content();
        #[cfg(feature = "profiling")]
        {
            result.trace_view = crate::profiling::targets::next_id();
            result.trace_lease = 0;
            result.trace_region = None;
            let mut data = result.trace_data();
            data.source_target = self.trace_id();
            data.source_view_id = self.trace_view_id();
            data.source_lease_id = self.trace_lease_id();
            data.source_extent = self.view.size();
            data.known |= crate::profiling::KNOWN_SOURCE_EXTENT;
            trace.complete(data);
        }
        Ok(result)
    }
    pub fn new(
        context: &super::ownership::Device,
        size: [u32; 2],
        format: native::TargetFormat,
        target_storage: native::TargetStorage,
        budget: &BudgetPool,
        limit: u64,
    ) -> Result<Self, (GpuError, Option<super::ownership::NativeTicket>)> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::TargetCreate);
        #[cfg(feature = "profiling")]
        let mut trace = crate::profiling::targets::Operation::new(
            "new",
            target_request_trace(size, format, target_storage),
        );
        let layout = target_storage.layout(size, format).map_err(|error| {
            #[cfg(feature = "profiling")]
            trace.fail(trace_native_code(&error));
            (error.into(), None)
        })?;
        let bytes = layout.texture_bytes;
        #[cfg(feature = "profiling")]
        {
            trace.data_mut().bytes = bytes;
            trace.data_mut().stride_bytes = layout.row_stride_bytes;
            trace.data_mut().known |= crate::profiling::KNOWN_STRIDE;
            trace.stage("budget");
        }
        let storage = {
            let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::TargetReserve);
            budget.reserve(bytes)
        }
        .map_err(|_| {
            (
                GpuError::Pressure {
                    domain: "offscreen targets",
                    requested: bytes,
                    available: limit.saturating_sub(budget.usage().bytes),
                },
                None,
            )
        })?;
        let storage = context.ticket(storage);
        let built = (|| {
            let view = {
                #[cfg(feature = "profiling")]
                trace.stage("texture");
                let _timing =
                    reel_core::profile::Scope::new(reel_core::profile::Phase::TargetTexture);
                context.create_target_texture_with_storage(size, format, target_storage)?
            };
            let framebuffer = {
                #[cfg(feature = "profiling")]
                trace.stage("framebuffer");
                let _timing =
                    reel_core::profile::Scope::new(reel_core::profile::Phase::TargetFramebuffer);
                context.framebuffer(&view)?
            };
            Ok::<_, native::Error>((view, framebuffer))
        })();
        let (view, framebuffer) = match built {
            Ok(value) => value,
            Err(error) => {
                #[cfg(feature = "profiling")]
                trace.fail(trace_native_code(&error));
                let error = GpuError::from(error);
                if error.is_memory_pressure() {
                    // Construction has issued no GPU work. Native owners have
                    // been dropped; the ticket still preserves a release fault.
                    drop(storage);
                    context.check().map_err(|error| (error, None))?;
                    return Err((error, None));
                }
                return Err((error, Some(storage)));
            }
        };
        reel_core::profile::count(reel_core::profile::Phase::TargetCreated);
        let result = Self {
            effect_cache_allowed: true,
            slot: 0,
            source_crop: None,
            view,
            framebuffer,
            size,
            _storage: Rc::new(TargetStorage {
                macrotile: None,
                _ticket: storage,
                effect_pins: std::cell::Cell::new(0),
                #[cfg(feature = "profiling")]
                trace_id: crate::profiling::targets::next_id(),
            }),
            effect_content: None,
            effect_pin: None,
            #[cfg(feature = "profiling")]
            trace_view: crate::profiling::targets::next_id(),
            #[cfg(feature = "profiling")]
            trace_lease: 0,
            #[cfg(feature = "profiling")]
            trace_region: None,
            #[cfg(feature = "profiling")]
            trace_cell: None,
        };
        #[cfg(feature = "profiling")]
        trace.complete(result.trace_data());
        Ok(result)
    }

    /// All cell views share exactly one physical pixel owner and budget charge.
    pub(super) fn macrotile(
        context: &super::ownership::Device,
        layout: native::MacrotileLayout,
        sizes: &[[u32; 2]],
        budget: &BudgetPool,
        limit: u64,
    ) -> Result<SmallVec<[Self; 4]>, (GpuError, Option<super::ownership::NativeTicket>)> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::TargetCreate);
        #[cfg(feature = "profiling")]
        let mut trace = {
            let mut data = target_request_trace(
                layout.size(),
                native::TargetFormat::Rgba8,
                native::TargetStorage::Linear,
            );
            data.count = sizes.len() as u64;
            crate::profiling::targets::Operation::new("macro-new", data)
        };
        let bytes = context
            .macrotile_target_footprint(layout, sizes)
            .map_err(|error| {
                #[cfg(feature = "profiling")]
                trace.fail(trace_native_code(&error));
                (error.into(), None)
            })?
            .texture
            .texture_bytes;
        #[cfg(feature = "profiling")]
        {
            trace.data_mut().bytes = bytes;
            trace.stage("macro-budget");
        }
        let ticket = {
            let _reserve = reel_core::profile::Scope::new(reel_core::profile::Phase::TargetReserve);
            budget.reserve(bytes)
        }
        .map_err(|_| {
            (
                GpuError::Pressure {
                    domain: "offscreen targets",
                    requested: bytes,
                    available: limit.saturating_sub(budget.usage().bytes),
                },
                None,
            )
        })?;
        let ticket = context.ticket(ticket);
        #[cfg(feature = "profiling")]
        trace.stage("macro-native-unsplit");
        let owner = match context.create_macrotile_target(layout, sizes) {
            Ok(owner) => owner,
            Err(error) => {
                #[cfg(feature = "profiling")]
                trace.fail(trace_native_code(&error));
                let error = GpuError::from(error);
                if error.is_memory_pressure() {
                    drop(ticket);
                    context.check().map_err(|error| (error, None))?;
                    return Err((error, None));
                }
                return Err((error, Some(ticket)));
            }
        };
        let storage = Rc::new(TargetStorage {
            macrotile: Some(owner),
            _ticket: ticket,
            effect_pins: std::cell::Cell::new(0),
            #[cfg(feature = "profiling")]
            trace_id: crate::profiling::targets::next_id(),
        });
        let cells: SmallVec<[Self; 4]> = (0..sizes.len())
            .map(|index| {
                let framebuffer = storage
                    .macrotile
                    .as_ref()
                    .unwrap()
                    .cell(index)
                    .expect("validated macrotile cell")
                    .clone();
                Self {
                    effect_cache_allowed: true,
                    slot: 0,
                    source_crop: None,
                    size: sizes[index],
                    view: framebuffer.texture().clone(),
                    framebuffer,
                    _storage: storage.clone(),
                    effect_content: None,
                    effect_pin: None,
                    #[cfg(feature = "profiling")]
                    trace_view: crate::profiling::targets::next_id(),
                    #[cfg(feature = "profiling")]
                    trace_lease: 0,
                    #[cfg(feature = "profiling")]
                    trace_region: None,
                    #[cfg(feature = "profiling")]
                    trace_cell: Some(index as u32),
                }
            })
            .collect();
        reel_core::profile::count(reel_core::profile::Phase::TargetCreated);
        #[cfg(feature = "profiling")]
        {
            let mut data = cells[0].trace_data();
            data.count = sizes.len() as u64;
            trace.complete(data);
            for cell in &cells {
                cell.trace_event(crate::profiling::TraceKind::TargetCache, "macro-cell-view");
            }
        }
        Ok(cells)
    }
}
