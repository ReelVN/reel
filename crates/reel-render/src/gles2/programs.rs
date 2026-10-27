use super::*;
use native::{
    AttributeBinding, Blend, Draw, Framebuffer, IndexBuffer, Program, ShaderCode,
    Target as NativeTarget, Texture, Uniform, UniformValue, VertexAttribute, VertexBuffer,
};
use smallvec::SmallVec;
#[cfg(target_os = "vita")]
macro_rules! shader_code {
    ($name:literal) => {
        ShaderCode::SgxBinary(include_bytes!(concat!(env!("OUT_DIR"), "/", $name, ".sgx")))
    };
}
#[cfg(not(target_os = "vita"))]
macro_rules! shader_code {
    ($name:literal) => {
        ShaderCode::Source(include_str!(concat!(env!("OUT_DIR"), "/", $name, ".glsl")))
    };
}
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
];
pub(super) type Values<'a> = SmallVec<[UniformValue<'a>; 24]>;
pub(super) struct Shader {
    pub program: Program,
    uniforms: [Option<Uniform>; NAMES.len()],
}
impl Shader {
    fn new(context: &native::Context, fragment: ShaderCode<'_>) -> Result<Self, GlesError> {
        Self::with_vertex(context, shader_code!("vertex"), fragment)
    }
    fn with_vertex(
        context: &native::Context,
        vertex: ShaderCode<'_>,
        fragment: ShaderCode<'_>,
    ) -> Result<Self, GlesError> {
        let program = create_program_with_vertex(context, vertex, fragment)?;
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
    context: &native::Context,
    fragment: ShaderCode<'_>,
    implicit_source: bool,
    padded: bool,
) -> Result<Program, GlesError> {
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
    )
}
fn create_program_with_vertex(
    context: &native::Context,
    vertex: ShaderCode<'_>,
    fragment: ShaderCode<'_>,
) -> Result<Program, GlesError> {
    let program = context.create_program(
        vertex,
        fragment,
        &[
            AttributeBinding {
                location: 0,
                name: "a_position",
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
        ],
    )?;
    Ok(program)
}
pub(super) struct Programs {
    pub rgba: Shader,
    pub direct: [Shader; 8],
    pub coverage: Shader,
    pub gradient: Shader,
    pub glyph_gradient: Shader,
    pub appearance: Shader,
    pub packed: Shader,
    pub video: Shader,
    pub video_appearance: Shader,
    pub composite: Shader,
    pub extended_blend: Shader,
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
    _storage: BudgetTicket,
}
impl Programs {
    pub fn new(context: &native::Context, budget: &BudgetPool) -> Result<Self, GlesError> {
        let bytes =
            (size_of::<Self>() + 4 * size_of::<crate::common::mesh::Vertex>() + 6 * 2) as u64;
        let storage = budget
            .reserve(bytes)
            .map_err(|e| GlesError::Device(e.message))?;
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
            rgba: Shader::new(context, shader_code!("paint_rgba"))?,
            direct: [
                Shader::new(context, shader_code!("paint_direct"))?,
                Shader::new(context, shader_code!("paint_direct_bc1"))?,
                Shader::new(context, shader_code!("paint_direct_bc3"))?,
                Shader::new(context, shader_code!("paint_regional"))?,
                Shader::new(context, shader_code!("paint_regional_bc1"))?,
                Shader::new(context, shader_code!("paint_regional_bc3"))?,
                Shader::new(context, shader_code!("paint_direct_coverage"))?,
                Shader::new(context, shader_code!("paint_solid"))?,
            ],
            coverage: Shader::new(context, shader_code!("paint_coverage"))?,
            gradient: Shader::new(context, shader_code!("paint_gradient"))?,
            glyph_gradient: Shader::new(context, shader_code!("paint_glyph_gradient"))?,
            appearance: Shader::new(context, shader_code!("paint_appearance"))?,
            packed: Shader::new(context, shader_code!("paint_packed"))?,
            video: Shader::new(context, shader_code!("paint_video"))?,
            video_appearance: Shader::new(context, shader_code!("paint_video_appearance"))?,
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
    pub fn quad(&self, context: &native::Context, quad: Quad<'_>) -> Result<(), GlesError> {
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
        context: &native::Context,
        target: &resources::Target,
        size: [u32; 2],
    ) -> Result<(), GlesError> {
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
#[derive(Clone)]
pub(super) struct Target {
    pub slot: usize,
    pub size: [u32; 2],
    pub view: Texture,
    pub framebuffer: Framebuffer,
    _storage: Rc<BudgetTicket>,
}
impl Target {
    pub fn source_uv(&self, parent: [u32; 2], origin: [i32; 2]) -> [f32; 4] {
        let storage = self.view.size();
        let mut uv = crate::common::source_view::uv_mapping(parent, storage, origin);
        uv[3] += 1. - self.size[1] as f32 / storage[1] as f32;
        uv
    }
    pub fn frame_uv(&self, parent: [u32; 2]) -> [f32; 4] {
        crate::common::source_view::uv_mapping(parent, self.view.size(), [0; 2])
    }
    pub fn same_allocation(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self._storage, &other._storage)
    }
    pub fn new(
        context: &native::Context,
        size: [u32; 2],
        budget: &BudgetPool,
        limit: u64,
    ) -> Result<Self, (GlesError, Option<BudgetTicket>)> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::TargetCreate);
        let bytes = u64::from(size[0]) * u64::from(size[1]) * 4;
        let storage = budget.reserve(bytes).map_err(|_| {
            (
                GlesError::Pressure {
                    domain: "offscreen targets",
                    requested: bytes,
                    available: limit.saturating_sub(budget.usage().bytes),
                },
                None,
            )
        })?;
        let built = (|| {
            let view = context.create_texture(size, native::TextureFormat::Rgba8)?;
            let framebuffer = context.framebuffer(&view)?;
            Ok::<_, native::Error>((view, framebuffer))
        })();
        let (view, framebuffer) = match built {
            Ok(value) => value,
            Err(error) => return Err((error.into(), Some(storage))),
        };
        Ok(Self {
            slot: 0,
            view,
            framebuffer,
            size,
            _storage: Rc::new(storage),
        })
    }
}
