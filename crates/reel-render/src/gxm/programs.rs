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
    overwrite: [std::cell::OnceCell<Shader>; 2],
    #[cfg(feature = "gxm-tile-destination")]
    tile: [std::cell::OnceCell<Shader>; 2],
    pub source_over: Shader,
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
            overwrite: std::array::from_fn(|_| std::cell::OnceCell::new()),
            #[cfg(feature = "gxm-tile-destination")]
            tile: std::array::from_fn(|_| std::cell::OnceCell::new()),
            paint: paint::PaintPrograms::new(budget),
            composite: Shader::with_vertex(
                context,
                shader_code!("composite_vertex"),
                shader_code!("composite"),
            )?,
            extended_blend: Shader::with_vertex(
                context,
                shader_code!("composite_vertex"),
                shader_code!("extended_blend"),
            )?,
            source_over: Shader::with_vertex(
                context,
                shader_code!("composite_vertex"),
                shader_code!("source_over"),
            )?,
            alpha_mask: Shader::with_vertex(
                context,
                shader_code!("composite_vertex"),
                shader_code!("alpha_mask"),
            )?,
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
            mask_sample_chain_composite: Shader::with_vertex(
                context,
                shader_code!("composite_vertex"),
                shader_code!("mask_sample_chain_composite"),
            )?,
            mask_threshold_composite: Shader::with_vertex(
                context,
                shader_code!("composite_vertex"),
                shader_code!("mask_threshold_composite"),
            )?,
            mask_paired: [
                Shader::new(context, shader_code!("mask_paired_aa"))?,
                Shader::new(context, shader_code!("mask_paired_ag"))?,
                Shader::new(context, shader_code!("mask_paired_ga"))?,
                Shader::new(context, shader_code!("mask_paired_gg"))?,
            ],
            mask_paired_composite: [
                Shader::with_vertex(
                    context,
                    shader_code!("composite_vertex"),
                    shader_code!("mask_paired_aa_composite"),
                )?,
                Shader::with_vertex(
                    context,
                    shader_code!("composite_vertex"),
                    shader_code!("mask_paired_ag_composite"),
                )?,
                Shader::with_vertex(
                    context,
                    shader_code!("composite_vertex"),
                    shader_code!("mask_paired_ga_composite"),
                )?,
                Shader::with_vertex(
                    context,
                    shader_code!("composite_vertex"),
                    shader_code!("mask_paired_gg_composite"),
                )?,
            ],
            present: Shader::new(context, shader_code!("present"))?,
            vertices: context.vertex_buffer(bytemuck::cast_slice(&data), 12, &attributes)?,
            indices: context.index_buffer_u16(&[0, 1, 2, 0, 2, 3])?,
            _storage: storage,
        })
    }
    #[cfg(feature = "gxm-tile-destination")]
    pub fn tile(
        &self,
        context: &super::ownership::Device,
        extended: bool,
    ) -> Result<&Shader, GpuError> {
        let slot = &self.tile[usize::from(extended)];
        if slot.get().is_none() {
            let program = create_program_with_output(
                context,
                shader_code!("composite_vertex"),
                if extended {
                    shader_code!("tile_extended_blend")
                } else {
                    shader_code!("tile_composite")
                },
                true,
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

    pub fn overwrite(
        &self,
        context: &super::ownership::Device,
        extended: bool,
        _: crate::common::target_writes::FullOverwrite,
    ) -> Result<&Shader, GpuError> {
        let slot = &self.overwrite[usize::from(extended)];
        if slot.get().is_none() {
            let shader = Shader::with_clipping(
                context,
                shader_code!("composite_vertex"),
                if extended {
                    shader_code!("extended_blend_overwrite")
                } else {
                    shader_code!("composite_overwrite")
                },
                false,
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
        context.draw(Draw {
            target,
            size,
            program: &shader.program,
            vertices: &self.vertices,
            instances: None,
            indices: Some(&self.indices),
            range: 0..6,
            textures,
            uniforms: values,
            blend,
            scissor,
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
    _ticket: super::ownership::NativeTicket,
    effect_pins: std::cell::Cell<usize>,
}
struct EffectPin {
    storage: Rc<TargetStorage>,
    token: crate::common::effect_result::Token,
    _metadata: BudgetTicket,
}
impl Drop for EffectPin {
    fn drop(&mut self) {
        self.storage
            .effect_pins
            .set(self.storage.effect_pins.get() - 1);
    }
}
#[derive(Clone)]
pub(super) struct Target {
    pub slot: usize,
    pub size: [u32; 2],
    pub view: Texture,
    pub framebuffer: Framebuffer,
    _storage: Rc<TargetStorage>,
    effect_pin: Option<Rc<EffectPin>>,
}
impl Target {
    pub(super) fn is_effect_pinned(&self) -> bool {
        self._storage.effect_pins.get() != 0
    }
    pub(super) fn effect_token(&self) -> Option<crate::common::effect_result::Token> {
        self.effect_pin.as_ref().map(|pin| pin.token)
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
        let mut result = self.clone();
        result.effect_pin = Some(Rc::new(EffectPin {
            storage: self._storage.clone(),
            token,
            _metadata: metadata,
        }));
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
    pub fn new(
        context: &super::ownership::Device,
        size: [u32; 2],
        format: native::TargetFormat,
        budget: &BudgetPool,
        limit: u64,
    ) -> Result<Self, (GpuError, Option<super::ownership::NativeTicket>)> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::TargetCreate);
        let bytes = context
            .target_layout(size, format)
            .map_err(|error| (error.into(), None))?
            .texture_bytes;
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
                let _timing =
                    reel_core::profile::Scope::new(reel_core::profile::Phase::TargetTexture);
                super::super::create_target_texture(context, size, format)?
            };
            let framebuffer = {
                let _timing =
                    reel_core::profile::Scope::new(reel_core::profile::Phase::TargetFramebuffer);
                context.framebuffer(&view)?
            };
            Ok::<_, native::Error>((view, framebuffer))
        })();
        let (view, framebuffer) = match built {
            Ok(value) => value,
            Err(error) => {
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
        Ok(Self {
            slot: 0,
            view,
            framebuffer,
            size,
            _storage: Rc::new(TargetStorage {
                _ticket: storage,
                effect_pins: std::cell::Cell::new(0),
            }),
            effect_pin: None,
        })
    }
}
