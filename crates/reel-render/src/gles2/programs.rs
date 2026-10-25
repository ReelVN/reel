use super::*;
use native::{
    AttributeBinding, Blend, Draw, Framebuffer, IndexBuffer, Program, ShaderCode,
    Target as NativeTarget, Texture, Uniform, UniformValue, VertexAttribute, VertexBuffer,
};
use smallvec::SmallVec;
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
];
pub(super) type Values<'a> = SmallVec<[UniformValue<'a>; 24]>;
pub(super) struct Shader {
    pub program: Program,
    uniforms: Vec<Option<Uniform>>,
}
impl Shader {
    fn new(context: &native::Context, bytes: &[u8]) -> Result<Self, GlesError> {
        let program = context.create_program(
            ShaderCode::SgxBinary(include_bytes!(concat!(env!("OUT_DIR"), "/vertex.sgx"))),
            ShaderCode::SgxBinary(bytes),
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
        let uniforms = NAMES.iter().map(|name| program.uniform(name)).collect();
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
pub(super) struct Programs {
    pub rgba: Shader,
    pub coverage: Shader,
    pub gradient: Shader,
    pub glyph_gradient: Shader,
    pub appearance: Shader,
    pub packed: Shader,
    pub video: Shader,
    pub video_appearance: Shader,
    pub composite: Shader,
    pub mask: Shader,
    pub present: Shader,
    pub vertices: VertexBuffer,
    pub indices: IndexBuffer,
    _storage: BudgetTicket,
}
impl Programs {
    pub fn new(context: &native::Context, budget: &BudgetPool) -> Result<Self, GlesError> {
        let bytes = (size_of::<Self>()
            + NAMES.len() * size_of::<Option<Uniform>>() * 11
            + 4 * size_of::<crate::common::mesh::Vertex>()
            + 6 * 4) as u64;
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
            rgba: Shader::new(
                context,
                include_bytes!(concat!(env!("OUT_DIR"), "/paint_rgba.sgx")),
            )?,
            coverage: Shader::new(
                context,
                include_bytes!(concat!(env!("OUT_DIR"), "/paint_coverage.sgx")),
            )?,
            gradient: Shader::new(
                context,
                include_bytes!(concat!(env!("OUT_DIR"), "/paint_gradient.sgx")),
            )?,
            glyph_gradient: Shader::new(
                context,
                include_bytes!(concat!(env!("OUT_DIR"), "/paint_glyph_gradient.sgx")),
            )?,
            appearance: Shader::new(
                context,
                include_bytes!(concat!(env!("OUT_DIR"), "/paint_appearance.sgx")),
            )?,
            packed: Shader::new(
                context,
                include_bytes!(concat!(env!("OUT_DIR"), "/paint_packed.sgx")),
            )?,
            video: Shader::new(
                context,
                include_bytes!(concat!(env!("OUT_DIR"), "/paint_video.sgx")),
            )?,
            video_appearance: Shader::new(
                context,
                include_bytes!(concat!(env!("OUT_DIR"), "/paint_video_appearance.sgx")),
            )?,
            composite: Shader::new(
                context,
                include_bytes!(concat!(env!("OUT_DIR"), "/composite.sgx")),
            )?,
            mask: Shader::new(
                context,
                include_bytes!(concat!(env!("OUT_DIR"), "/mask.sgx")),
            )?,
            present: Shader::new(
                context,
                include_bytes!(concat!(env!("OUT_DIR"), "/present.sgx")),
            )?,
            vertices: context.vertex_buffer(bytemuck::cast_slice(&data), 12, &attributes)?,
            indices: context.index_buffer(&[0, 1, 2, 0, 2, 3])?,
            _storage: storage,
        })
    }
    pub fn quad(
        &self,
        context: &native::Context,
        target: NativeTarget<'_>,
        size: [u32; 2],
        shader: &Shader,
        textures: &[&Texture],
        values: &[UniformValue<'_>],
        blend: Blend,
        scissor: Option<[u32; 4]>,
    ) -> Result<(), GlesError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Draw);
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
        let mut values = Values::new();
        values.extend(self.present.int("u_image", 0));
        self.quad(
            context,
            NativeTarget::Screen,
            size,
            &self.present,
            &[&target.view],
            &values,
            Blend::Replace,
            None,
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
    pub view: Texture,
    pub framebuffer: Framebuffer,
    _storage: Rc<BudgetTicket>,
}
impl Target {
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
            view,
            framebuffer,
            _storage: Rc::new(storage),
        })
    }
}
