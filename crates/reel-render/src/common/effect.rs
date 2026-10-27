//! Static shader generation for the shared typed drawing-expression graph.
//!
//! [`compile_fragment`] supplies the production source hook and entry point;
//! [`generate`] exposes the same expression body for CPU tooling. Source reads
//! return linear premultiplied color at base level, including nonuniform branches.
//!
//! Both languages use float32 operations. Remainder lowers to
//! `a - b * (sign(a / b) * floor(abs(a / b)))`, with the quotient evaluated once;
//! this is a truncating-quotient expression, not a bit-exact IEEE fmod routine.
//! Smoothstep uses a step at equal edges and the same clamped cubic for ordered
//! and reversed edges. Hardware transcendental precision remains target-specific.

use reel_ui::stage::effect::{
    EffectBinary, EffectInput, EffectNodeId, EffectOp, EffectProgram, EffectTernary,
    EffectTextureChannel, EffectType, EffectUnary, EffectValue,
};
use std::collections::{BTreeMap, HashMap};

/// The production source binding is nine float32 vec4 slots (144 bytes).
/// Slots 0/1 map target pixels to local normalized coordinates; 2/3 map local
/// normalized coordinates to source texture coordinates. Slot 4 is the logical
/// rectangle (x, y, width, height); 5 is target size and source storage size;
/// 6 is the source atlas window (origin x/y, extent x/y), in normalized units.
/// Slots 7/8 map normalized target coordinates directly to the whole local
/// source for the wrapper's implicit current-pixel sample.
/// Slot 0.w selects ordinary output (zero), scalar coverage (one), or inverted
/// scalar coverage (two), or source multiplied by inverted coverage (three).
/// Inversion follows coverage clamping.
pub const CONTEXT_SLOTS: usize = 9;

/// Complete prepared shader source; runtime backends load this product verbatim.
#[derive(Clone, Debug)]
pub struct EffectFragment {
    pub source: String,
    pub parameters: ParameterLayout,
    pub output_type: EffectType,
    /// Whether the complete entry point references the subtree source sampler.
    pub source_sampler: bool,
}

/// Export the complete v1 fragment ABI without requiring a GPU feature.
///
/// GLES uses the renderer's `a_position/a_uv` inputs, `v_uv`, sampler
/// `reel_source`, `reel_context[9]`, and individual vec4
/// uniforms `reel_parameter_N`. Source targets have only base level, so ordinary
/// texture2D samples level zero even in nonuniform branches. WGSL includes the
/// fullscreen `vertex` and `fragment` entries and group zero bindings: source
/// texture 0 when used, linear clamp sampler 1, context 2, numeric parameters 3,
/// texture metadata 4, and fixed external texture bindings starting at 5. Texture
/// parameters have no numeric uniform slots. Each image has content scale,
/// encoding/format flags and half-texel bounds; generated channel helpers access
/// only the metadata required by their declared sample operations.
///
/// Output is restricted to the local logical rectangle. Source samples clamp
/// normalized local coordinates and the source atlas window at texel centers.
/// Source and output are linear premultiplied colors; scalar output multiplies
/// the unmodified whole local source at the current coordinates by coverage.
/// That implicit sample uses an affine `reel_implicit_source_uv` vertex varying
/// and hardware edge clamping. Padded scratch supplies piecewise clamped vertex UVs.
/// Explicit authored samples retain their fragment
/// coordinate evaluation and texel/window clamps.
pub fn compile_fragment(
    program: &EffectProgram,
    language: Language,
    max_source_bytes: usize,
) -> Result<EffectFragment, GenerateError> {
    let generated = generate(program, language, max_source_bytes)?;
    let source_sampler = generated.samples_source || generated.output_type == EffectType::Float;
    let mut source = String::new();
    match language {
        Language::GlslEs100 => {
            source.push_str("#version 100\nprecision highp float;\nvarying vec2 v_uv;\nuniform sampler2D reel_source;\nuniform vec4 reel_context[9];\n");
            if generated.output_type == EffectType::Float {
                source.push_str("varying vec2 reel_implicit_source_uv;\n");
            }
            source.push_str(&generated.declarations);
            source.push_str(r#"
vec4 reel_source_sample(vec2 uv) {
    vec3 p = vec3(clamp(uv, 0.0, 1.0), 1.0);
    vec2 q = vec2(dot(reel_context[2].xyz, p), dot(reel_context[3].xyz, p));
    vec2 half_texel = 0.5 / reel_context[5].zw;
    q = clamp(q, reel_context[6].xy + half_texel, reel_context[6].xy + reel_context[6].zw - half_texel);
    return texture2D(reel_source, vec2(q.x, 1.0 - q.y));
}
"#);
            source.push_str(&texture_functions(
                program,
                language,
                max_source_bytes.saturating_sub(source.len() + generated.function.len()),
            )?);
            source.push_str(&generated.function);
            source.push_str("\nvoid main() {\n");
            if generated.output_type == EffectType::Float {
                source.push_str("vec4 reel_implicit_source = texture2D(reel_source, reel_implicit_source_uv);\n");
            }
            source.push_str(
                r#"
    vec3 p = vec3(v_uv * reel_context[5].xy, 1.0);
    vec2 uv = vec2(dot(reel_context[0].xyz, p), dot(reel_context[1].xyz, p));
    if (any(lessThan(uv, vec2(0.0))) || any(greaterThan(uv, vec2(1.0)))) discard;
    vec2 position = reel_context[4].xy + uv * reel_context[4].zw;
"#,
            );
            if generated.output_type == EffectType::Float {
                source.push_str("float coverage = clamp(reel_effect(uv, position, reel_context[4].zw), 0.0, 1.0); float mode = reel_context[0].w; if (mode >= 2.0) { coverage = 1.0 - coverage; } if (mode == 1.0 || mode == 2.0) { gl_FragColor = vec4(coverage); } else { gl_FragColor = reel_implicit_source * coverage; }\n}\n");
            } else {
                source
                    .push_str("gl_FragColor = reel_effect(uv, position, reel_context[4].zw);\n}\n");
            }
        }
        Language::Wgsl => {
            source.push_str(
                r#"
struct ReelEffectContext { slots: array<vec4<f32>, 9> }
@group(0) @binding(0) var reel_source: texture_2d<f32>;
@group(0) @binding(1) var reel_sampler: sampler;
@group(0) @binding(2) var<uniform> reel_context: ReelEffectContext;
"#,
            );
            if generated.parameters.slots.is_empty() {
                source.push_str("struct ReelEffectParameters { unused: vec4<f32> }\n");
            } else {
                source.push_str(&generated.declarations);
            }
            source.push_str(r#"
@group(0) @binding(3) var<uniform> reel_parameters: ReelEffectParameters;
fn reel_source_sample(uv: vec2<f32>) -> vec4<f32> {
    let p = vec3(clamp(uv, vec2(0.0), vec2(1.0)), 1.0);
    let q = vec2(dot(reel_context.slots[2].xyz, p), dot(reel_context.slots[3].xyz, p));
    let half_texel = vec2(0.5) / reel_context.slots[5].zw;
    let sampled = clamp(q, reel_context.slots[6].xy + half_texel, reel_context.slots[6].xy + reel_context.slots[6].zw - half_texel);
    return textureSampleLevel(reel_source, reel_sampler, sampled, 0.0);
}
"#);
            if generated.output_type == EffectType::Float {
                source.push_str("struct ReelEffectVarying { @builtin(position) position: vec4<f32>, @location(0) implicit_source_uv: vec2<f32> }\n");
            } else {
                source.push_str(
                    "struct ReelEffectVarying { @builtin(position) position: vec4<f32> }\n",
                );
            }
            source.push_str(
                r#"
@vertex fn vertex(@builtin(vertex_index) i: u32) -> ReelEffectVarying {
    let uv = vec2(f32((i << 1u) & 2u), f32(i & 2u));
    var out: ReelEffectVarying;
    out.position = vec4(uv * vec2(2.0, -2.0) + vec2(-1.0, 1.0), 0.0, 1.0);
"#,
            );
            if generated.output_type == EffectType::Float {
                source.push_str("let p = vec3(uv, 1.0);\nout.implicit_source_uv = vec2(dot(reel_context.slots[7].xyz, p), dot(reel_context.slots[8].xyz, p));\n");
            }
            source.push_str(
                r#"
    return out;
}
"#,
            );
            if generated.output_type == EffectType::Float {
                source.push_str("@vertex fn vertex_padded(@location(0) position: vec2<f32>, @location(1) source_uv: vec2<f32>) -> ReelEffectVarying {\nvar out: ReelEffectVarying;\nout.position = vec4(position, 0.0, 1.0);\nout.implicit_source_uv = source_uv;\nreturn out;\n}\n");
            }
            source.push_str(&texture_functions(
                program,
                language,
                max_source_bytes.saturating_sub(source.len() + generated.function.len()),
            )?);
            source.push_str(&generated.function);
            source.push_str(
                "\n@fragment fn fragment(v: ReelEffectVarying) -> @location(0) vec4<f32> {\n",
            );
            if generated.output_type == EffectType::Float {
                source.push_str("let reel_implicit_source = textureSampleLevel(reel_source, reel_sampler, v.implicit_source_uv, 0.0);\n");
            }
            source.push_str(
                r#"
    let p = vec3(v.position.xy, 1.0);
    let uv = vec2(dot(reel_context.slots[0].xyz, p), dot(reel_context.slots[1].xyz, p));
    if any(uv < vec2(0.0)) || any(uv > vec2(1.0)) { discard; }
    let position = reel_context.slots[4].xy + uv * reel_context.slots[4].zw;
"#,
            );
            if generated.output_type == EffectType::Float {
                source.push_str("var coverage = clamp(reel_effect(uv, position, reel_context.slots[4].zw), 0.0, 1.0); let mode = reel_context.slots[0].w; if mode >= 2.0 { coverage = 1.0 - coverage; } if mode == 1.0 || mode == 2.0 { return vec4(coverage); } return reel_implicit_source * coverage;\n}\n");
            } else {
                source.push_str("return reel_effect(uv, position, reel_context.slots[4].zw);\n}\n");
            }
        }
    }
    if !source_sampler {
        match language {
            Language::GlslEs100 => {
                source = source.replace("uniform sampler2D reel_source;\n", "");
                let start = source.find("vec4 reel_source_sample(").unwrap();
                let end = source[start..].find("\n}\n").unwrap() + start + 3;
                source.replace_range(start..end, "");
            }
            Language::Wgsl => {
                source = source.replace(
                    "@group(0) @binding(0) var reel_source: texture_2d<f32>;\n",
                    "",
                );
                let start = source.find("fn reel_source_sample(").unwrap();
                let end = source[start..].find("\n}\n").unwrap() + start + 3;
                source.replace_range(start..end, "");
            }
        }
    }
    if source.len() > max_source_bytes {
        return Err(GenerateError::SourceLimit {
            limit: max_source_bytes,
        });
    }
    Ok(EffectFragment {
        source,
        parameters: generated.parameters,
        output_type: generated.output_type,
        source_sampler,
    })
}

/// Fixed sampler helpers follow the graph's actual channel uses; no GPU opcode
/// or resource-index interpreter is introduced by external image parameters.
fn texture_functions(
    program: &EffectProgram,
    language: Language,
    limit: usize,
) -> Result<String, GenerateError> {
    use std::fmt::Write;
    let count = program.textures().len();
    if count == 0 {
        return Ok(String::new());
    }
    if count > limit / 16 {
        return Err(GenerateError::SourceLimit { limit });
    }
    let mut channel_uses = vec![0_u8; count];
    for node in program.nodes() {
        if let EffectOp::TextureSample {
            binding, channel, ..
        } = node.op
        {
            channel_uses[binding as usize] |= match channel {
                EffectTextureChannel::Color => 1,
                EffectTextureChannel::Alpha => 2,
                EffectTextureChannel::Gray => 4,
            };
        }
    }
    let mut code = String::new();
    match language {
        Language::GlslEs100 => {
            writeln!(code, "uniform vec4 reel_texture_meta[{}];", count * 2).unwrap();
        }
        Language::Wgsl => {
            writeln!(code, "struct ReelTextureMetadata {{ values: array<vec4<f32>,{}> }}\n@group(0) @binding(4) var<uniform> reel_texture_meta: ReelTextureMetadata;", count * 2).unwrap();
        }
    }
    let gray = channel_uses.iter().any(|usage| usage & 4 != 0);
    if gray {
        code.push_str(match language {
            Language::GlslEs100 => "vec3 reel_texture_encode(vec3 x){return mix(1.055*pow(max(x,vec3(0.0)),vec3(1.0/2.4))-0.055,12.92*x,step(x,vec3(0.0031308)));}\n",
            Language::Wgsl => "fn reel_texture_encode(x:vec3<f32>)->vec3<f32>{return select(1.055*pow(max(x,vec3(0.0)),vec3(1.0/2.4))-0.055,12.92*x,x<=vec3(0.0031308));}\n",
        });
    }
    for (binding, &channels) in channel_uses.iter().enumerate().take(count) {
        if channels == 0 {
            continue;
        }
        match language {
            Language::GlslEs100 => {
                writeln!(code, "uniform sampler2D reel_texture_{binding};\nvec4 reel_texture_{binding}_read(vec2 uv){{vec4 t=reel_texture_meta[{}];vec2 h=reel_texture_meta[{}].xy;vec4 s=texture2D(reel_texture_{binding},clamp(uv*t.xy,h,t.xy-h));if(t.w==1.0)s.a=1.0;if(t.w==2.0)s.rgb=min(s.rgb,vec3(s.a));return s;}}", binding*2,binding*2+1).unwrap();
                if channels & 1 != 0 {
                    writeln!(code,"vec4 reel_texture_{binding}_color(vec2 uv){{return reel_texture_{binding}_read(uv);}}").unwrap();
                }
                if channels & 2 != 0 {
                    writeln!(code,"float reel_texture_{binding}_alpha(vec2 uv){{return reel_texture_{binding}_read(uv).a;}}").unwrap();
                }
                if channels & 4 != 0 {
                    writeln!(code,"float reel_texture_{binding}_gray(vec2 uv){{vec4 s=reel_texture_{binding}_read(uv);vec3 rgb=s.a>0.0?clamp(s.rgb/s.a,0.0,1.0):vec3(0.0);if(reel_texture_meta[{}].z==0.0)rgb=reel_texture_encode(rgb);return dot(rgb,vec3(0.2126,0.7152,0.0722));}}",binding*2).unwrap();
                }
            }
            Language::Wgsl => {
                writeln!(code,"@group(0) @binding({}) var reel_texture_{binding}:texture_2d<f32>;\nfn reel_texture_{binding}_read(uv:vec2<f32>)->vec4<f32>{{let t=reel_texture_meta.values[{}];let h=reel_texture_meta.values[{}].xy;var s=textureSampleLevel(reel_texture_{binding},reel_sampler,clamp(uv*t.xy,h,t.xy-h),0.0);if t.w==1.0{{s.a=1.0;}}if t.w==2.0{{s=vec4(min(s.rgb,vec3(s.a)),s.a);}}return s;}}",5+binding,binding*2,binding*2+1).unwrap();
                if channels & 1 != 0 {
                    writeln!(code,"fn reel_texture_{binding}_color(uv:vec2<f32>)->vec4<f32>{{return reel_texture_{binding}_read(uv);}}").unwrap();
                }
                if channels & 2 != 0 {
                    writeln!(code,"fn reel_texture_{binding}_alpha(uv:vec2<f32>)->f32{{return reel_texture_{binding}_read(uv).a;}}").unwrap();
                }
                if channels & 4 != 0 {
                    writeln!(code,"fn reel_texture_{binding}_gray(uv:vec2<f32>)->f32{{let s=reel_texture_{binding}_read(uv);var rgb=vec3(0.0);if s.a>0.0{{rgb=clamp(s.rgb/s.a,vec3(0.0),vec3(1.0));}}if reel_texture_meta.values[{}].z==0.0{{rgb=reel_texture_encode(rgb);}}return dot(rgb,vec3(0.2126,0.7152,0.0722));}}",binding*2).unwrap();
                }
            }
        }
        if code.len() > limit {
            return Err(GenerateError::SourceLimit { limit });
        }
    }
    if code.len() > limit {
        return Err(GenerateError::SourceLimit { limit });
    }
    Ok(code)
}

/// The shader language of a prepared pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    GlslEs100,
    Wgsl,
}

/// One stable uniform slot. Values occupy the first lanes of a sixteen-byte slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParameterSlot {
    /// Type checked before host values are packed into this slot.
    pub ty: EffectType,
    /// Byte offset from the start of the parameter uniform buffer.
    pub byte_offset: usize,
}

/// Shared host layout; WGSL members and GLES vec4 uniforms use identical lanes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParameterLayout {
    pub slots: Vec<ParameterSlot>,
}

impl ParameterLayout {
    /// Total bytes occupied by the declared sixteen-byte slots.
    pub fn byte_len(&self) -> usize {
        self.slots.len() * 16
    }

    /// Pack already-resolved parameter values; booleans occupy x as zero or one.
    pub fn pack(&self, values: &[EffectValue]) -> Result<Vec<[f32; 4]>, ParameterError> {
        if values.len() != self.slots.len() {
            return Err(ParameterError::Count);
        }
        values
            .iter()
            .zip(&self.slots)
            .enumerate()
            .map(|(index, (&value, slot))| {
                if value.ty() != slot.ty {
                    return Err(ParameterError::Type { index });
                }
                if !value.is_finite() {
                    return Err(ParameterError::NonFinite { index });
                }
                Ok(match value {
                    EffectValue::Bool(value) => [u8::from(value) as f32, 0., 0., 0.],
                    EffectValue::Float(a) => [f32::from_bits(a), 0., 0., 0.],
                    EffectValue::Vec2([a, b]) => [f32::from_bits(a), f32::from_bits(b), 0., 0.],
                    EffectValue::Vec3([a, b, c]) => {
                        [f32::from_bits(a), f32::from_bits(b), f32::from_bits(c), 0.]
                    }
                    EffectValue::Vec4(bits) => bits.map(f32::from_bits),
                })
            })
            .collect()
    }
}

/// A resolved value does not match the immutable uniform layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParameterError {
    Count,
    Type { index: usize },
    NonFinite { index: usize },
}
impl std::fmt::Display for ParameterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Count => f.write_str("effect parameter count differs from the uniform layout"),
            Self::Type { index } => write!(
                f,
                "effect parameter {index} has a different type from its uniform slot"
            ),
            Self::NonFinite { index } => {
                write!(f, "effect parameter {index} has a nonfinite component")
            }
        }
    }
}
impl std::error::Error for ParameterError {}

/// Shader snippets ready for the backend's source hook and fragment wrapper.
#[derive(Clone, Debug)]
pub struct GeneratedEffect {
    pub parameters: ParameterLayout,
    /// GLSL uniform declarations or a WGSL `ReelEffectParameters` struct.
    /// WGSL consumers declare their own binding named `reel_parameters`.
    pub declarations: String,
    pub function: String,
    pub output_type: EffectType,
    pub samples_source: bool,
}

/// Source expansion is bounded before a driver/compiler sees the generated code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GenerateError {
    SourceLimit { limit: usize },
}
impl std::fmt::Display for GenerateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceLimit { limit } => {
                write!(f, "effect shader exceeds its {limit}-byte source limit")
            }
        }
    }
}
impl std::error::Error for GenerateError {}

/// Generate static expressions and structured branches, never a pixel interpreter.
///
/// Nodes shared within a dominating scope are evaluated once. Branch-local nodes
/// stay inside their selected branch, including source reads and undefined math.
/// Traversal uses an explicit CPU work stack so graph depth cannot overflow it.
/// The byte limit covers the returned declarations and function, including helpers;
/// backend-owned entry points and source hooks have their own admission budget.
pub fn generate(
    program: &EffectProgram,
    language: Language,
    max_source_bytes: usize,
) -> Result<GeneratedEffect, GenerateError> {
    // Every declared parameter needs at least one sixteen-byte slot and more
    // than sixteen source bytes. Reject oversized layouts before allocating them.
    if program.parameters().len() > max_source_bytes / 16 {
        return Err(GenerateError::SourceLimit {
            limit: max_source_bytes,
        });
    }
    let parameters = ParameterLayout {
        slots: program
            .parameters()
            .iter()
            .enumerate()
            .map(|(index, p)| ParameterSlot {
                ty: p.ty,
                byte_offset: index * 16,
            })
            .collect(),
    };
    let mut declarations = String::new();
    if !parameters.slots.is_empty() {
        if language == Language::Wgsl {
            declarations.push_str("struct ReelEffectParameters {\n");
        }
        for index in 0..parameters.slots.len() {
            declarations.push_str(&match language {
                Language::GlslEs100 => format!("uniform vec4 reel_parameter_{index};\n"),
                Language::Wgsl => format!("    p{index}: vec4<f32>,\n"),
            });
            if declarations.len() > max_source_bytes {
                return Err(GenerateError::SourceLimit {
                    limit: max_source_bytes,
                });
            }
        }
        if language == Language::Wgsl {
            declarations.push_str("}\n");
        }
    }
    if declarations.len() > max_source_bytes {
        return Err(GenerateError::SourceLimit {
            limit: max_source_bytes,
        });
    }
    let mut emitter = Emitter {
        program,
        language,
        max_bytes: max_source_bytes - declarations.len(),
        limit: max_source_bytes,
        body: String::new(),
        helpers: BTreeMap::new(),
        helper_bytes: 0,
        scopes: vec![Scope::default()],
        next_name: 0,
        samples_source: false,
    };
    emitter.run()?;
    let output_type = program.nodes()[program.output().0 as usize].ty;
    let return_type = language.ty(output_type);
    let header = match language {
        Language::GlslEs100 => {
            format!("{return_type} reel_effect(vec2 uv, vec2 position, vec2 size) {{\n")
        }
        Language::Wgsl => format!(
            "fn reel_effect(uv: vec2<f32>, position: vec2<f32>, size: vec2<f32>) -> {return_type} {{\n"
        ),
    };
    let result = emitter.lookup(0, program.output()).unwrap();
    emitter.line(0, &format!("return reel_n{result};"))?;
    let mut function = emitter.helpers.into_values().collect::<String>();
    function.push_str(&header);
    function.push_str(&emitter.body);
    function.push_str("}\n");
    if declarations.len() + function.len() > max_source_bytes {
        return Err(GenerateError::SourceLimit {
            limit: max_source_bytes,
        });
    }
    Ok(GeneratedEffect {
        parameters,
        declarations,
        function,
        output_type,
        samples_source: emitter.samples_source,
    })
}

impl Language {
    fn ty(self, ty: EffectType) -> &'static str {
        match (self, ty) {
            (_, EffectType::Bool) => "bool",
            (Self::GlslEs100, EffectType::Float) => "float",
            (Self::GlslEs100, EffectType::Vec2) => "vec2",
            (Self::GlslEs100, EffectType::Vec3) => "vec3",
            (Self::GlslEs100, EffectType::Vec4) => "vec4",
            (Self::Wgsl, EffectType::Float) => "f32",
            (Self::Wgsl, EffectType::Vec2) => "vec2<f32>",
            (Self::Wgsl, EffectType::Vec3) => "vec3<f32>",
            (Self::Wgsl, EffectType::Vec4) => "vec4<f32>",
        }
    }

    fn declaration(self, ty: EffectType, name: usize, expression: &str) -> String {
        let ty = self.ty(ty);
        match self {
            Self::GlslEs100 => format!("{ty} reel_n{name} = {expression};"),
            Self::Wgsl => format!("let reel_n{name}: {ty} = {expression};"),
        }
    }

    fn mutable(self, ty: EffectType, name: usize) -> String {
        let ty = self.ty(ty);
        match self {
            Self::GlslEs100 => format!("{ty} reel_n{name};"),
            Self::Wgsl => format!("var reel_n{name}: {ty};"),
        }
    }
}

#[derive(Default)]
struct Scope {
    parent: Option<usize>,
    depth: usize,
    values: HashMap<EffectNodeId, usize>,
}

enum Task {
    Need(usize, EffectNodeId),
    Finish(usize, EffectNodeId),
    Branch(usize, EffectNodeId),
    Otherwise {
        parent: usize,
        scope: usize,
        id: EffectNodeId,
        name: usize,
    },
    Join {
        parent: usize,
        scope: usize,
        id: EffectNodeId,
        name: usize,
    },
}

struct Emitter<'a> {
    program: &'a EffectProgram,
    language: Language,
    max_bytes: usize,
    limit: usize,
    body: String,
    helpers: BTreeMap<String, String>,
    helper_bytes: usize,
    scopes: Vec<Scope>,
    next_name: usize,
    samples_source: bool,
}

impl Emitter<'_> {
    fn lookup(&self, mut scope: usize, id: EffectNodeId) -> Option<usize> {
        loop {
            let current = &self.scopes[scope];
            if let Some(value) = current.values.get(&id) {
                return Some(*value);
            }
            scope = current.parent?;
        }
    }

    fn value(&self, scope: usize, id: EffectNodeId) -> String {
        format!("reel_n{}", self.lookup(scope, id).unwrap())
    }

    fn line(&mut self, scope: usize, text: &str) -> Result<(), GenerateError> {
        let indent = (self.scopes[scope].depth + 1) * 4;
        if self
            .body
            .len()
            .saturating_add(self.helper_bytes)
            .saturating_add(indent)
            .saturating_add(text.len() + 1)
            > self.max_bytes
        {
            return Err(GenerateError::SourceLimit { limit: self.limit });
        }
        self.body.extend(std::iter::repeat_n(' ', indent));
        self.body.push_str(text);
        self.body.push('\n');
        Ok(())
    }

    fn child(&mut self, parent: usize) -> usize {
        let index = self.scopes.len();
        self.scopes.push(Scope {
            parent: Some(parent),
            depth: self.scopes[parent].depth + 1,
            values: HashMap::new(),
        });
        index
    }

    fn run(&mut self) -> Result<(), GenerateError> {
        let mut tasks = vec![Task::Need(0, self.program.output())];
        while let Some(task) = tasks.pop() {
            match task {
                Task::Need(scope, id) => {
                    if self.lookup(scope, id).is_some() {
                        continue;
                    }
                    let op = &self.program.nodes()[id.0 as usize].op;
                    if let EffectOp::Select { condition, .. } = *op {
                        tasks.push(Task::Branch(scope, id));
                        tasks.push(Task::Need(scope, condition));
                    } else {
                        tasks.push(Task::Finish(scope, id));
                        for dependency in dependencies(op).into_iter().rev() {
                            tasks.push(Task::Need(scope, dependency));
                        }
                    }
                }
                Task::Finish(scope, id) => {
                    let node = self.program.nodes()[id.0 as usize].clone();
                    let expression = self.expression(scope, node.ty, &node.op)?;
                    let name = self.next_name;
                    self.next_name += 1;
                    self.line(
                        scope,
                        &self.language.declaration(node.ty, name, &expression),
                    )?;
                    self.scopes[scope].values.insert(id, name);
                }
                Task::Branch(parent, id) => {
                    let node = &self.program.nodes()[id.0 as usize];
                    let EffectOp::Select {
                        condition,
                        when_true,
                        ..
                    } = node.op
                    else {
                        unreachable!()
                    };
                    let name = self.next_name;
                    self.next_name += 1;
                    self.line(parent, &self.language.mutable(node.ty, name))?;
                    self.line(
                        parent,
                        &format!("if ({}) {{", self.value(parent, condition)),
                    )?;
                    let scope = self.child(parent);
                    tasks.push(Task::Otherwise {
                        parent,
                        scope,
                        id,
                        name,
                    });
                    tasks.push(Task::Need(scope, when_true));
                }
                Task::Otherwise {
                    parent,
                    scope,
                    id,
                    name,
                } => {
                    let EffectOp::Select {
                        when_true,
                        when_false,
                        ..
                    } = self.program.nodes()[id.0 as usize].op
                    else {
                        unreachable!()
                    };
                    self.line(
                        scope,
                        &format!("reel_n{name} = {};", self.value(scope, when_true)),
                    )?;
                    self.line(parent, "} else {")?;
                    let scope = self.child(parent);
                    tasks.push(Task::Join {
                        parent,
                        scope,
                        id,
                        name,
                    });
                    tasks.push(Task::Need(scope, when_false));
                }
                Task::Join {
                    parent,
                    scope,
                    id,
                    name,
                } => {
                    let EffectOp::Select { when_false, .. } =
                        self.program.nodes()[id.0 as usize].op
                    else {
                        unreachable!()
                    };
                    self.line(
                        scope,
                        &format!("reel_n{name} = {};", self.value(scope, when_false)),
                    )?;
                    self.line(parent, "}")?;
                    self.scopes[parent].values.insert(id, name);
                }
            }
        }
        Ok(())
    }

    fn expression(
        &mut self,
        scope: usize,
        ty: EffectType,
        op: &EffectOp,
    ) -> Result<String, GenerateError> {
        let value = |id| self.value(scope, id);
        Ok(match *op {
            EffectOp::Constant(value) => constant(self.language, value),
            EffectOp::Parameter(index) => {
                let name = match self.language {
                    Language::GlslEs100 => format!("reel_parameter_{index}"),
                    Language::Wgsl => format!("reel_parameters.p{index}"),
                };
                match ty {
                    EffectType::Bool => format!("({name}.x != 0.0)"),
                    EffectType::Float => format!("{name}.x"),
                    EffectType::Vec2 => format!("{name}.xy"),
                    EffectType::Vec3 => format!("{name}.xyz"),
                    EffectType::Vec4 => name,
                }
            }
            EffectOp::Input(input) => match input {
                EffectInput::Uv => "uv",
                EffectInput::Position => "position",
                EffectInput::Size => "size",
            }
            .into(),
            EffectOp::TextureSample {
                binding,
                uv,
                channel,
            } => {
                let suffix = match channel {
                    EffectTextureChannel::Color => "color",
                    EffectTextureChannel::Alpha => "alpha",
                    EffectTextureChannel::Gray => "gray",
                };
                format!("reel_texture_{binding}_{suffix}({})", value(uv))
            }
            EffectOp::SourceSample { uv } => {
                let uv = value(uv);
                self.samples_source = true;
                format!("reel_source_sample({uv})")
            }
            EffectOp::Vec2(ids) => construct(self.language, ty, ids.map(value).as_slice()),
            EffectOp::Vec3(ids) => construct(self.language, ty, ids.map(value).as_slice()),
            EffectOp::Vec4(ids) => construct(self.language, ty, ids.map(value).as_slice()),
            EffectOp::Extract { value: id, lane } => {
                format!("{}.{}", value(id), ["x", "y", "z", "w"][lane as usize])
            }
            EffectOp::Unary { op, value: id } => {
                let v = value(id);
                match op {
                    EffectUnary::Negate => format!("(-{v})"),
                    EffectUnary::Not => format!("(!{v})"),
                    _ => format!("{}({v})", unary_name(op)),
                }
            }
            EffectOp::Binary { op, left, right } => {
                let a = value(left);
                let b = value(right);
                let operand_ty = self.program.nodes()[left.0 as usize].ty;
                match op {
                    EffectBinary::Remainder => {
                        let helper = self.helper("remainder", operand_ty)?;
                        format!("{helper}({a}, {b})")
                    }
                    EffectBinary::Equal | EffectBinary::NotEqual
                        if matches!(
                            operand_ty,
                            EffectType::Vec2 | EffectType::Vec3 | EffectType::Vec4
                        ) =>
                    {
                        let equal = op == EffectBinary::Equal;
                        match self.language {
                            Language::GlslEs100 => format!(
                                "{}({}({a}, {b}))",
                                if equal { "all" } else { "any" },
                                if equal { "equal" } else { "notEqual" }
                            ),
                            Language::Wgsl => format!(
                                "{}({a} {} {b})",
                                if equal { "all" } else { "any" },
                                if equal { "==" } else { "!=" }
                            ),
                        }
                    }
                    EffectBinary::Atan2 => format!(
                        "{}({a}, {b})",
                        if self.language == Language::GlslEs100 {
                            "atan"
                        } else {
                            "atan2"
                        }
                    ),
                    EffectBinary::Pow
                    | EffectBinary::Min
                    | EffectBinary::Max
                    | EffectBinary::Dot
                    | EffectBinary::Distance => format!("{}({a}, {b})", binary_name(op)),
                    _ => format!("({a} {} {b})", binary_name(op)),
                }
            }
            EffectOp::Ternary { op, a, b, c } => {
                let args = [value(a), value(b), value(c)];
                let name = match op {
                    EffectTernary::Clamp => "clamp".into(),
                    EffectTernary::Mix => "mix".into(),
                    EffectTernary::Smoothstep => self.helper("smoothstep", ty)?,
                };
                format!("{name}({})", args.join(", "))
            }
            EffectOp::Select { .. } => unreachable!("selections are structured statements"),
        })
    }

    fn helper(&mut self, operation: &str, ty: EffectType) -> Result<String, GenerateError> {
        let suffix = match ty {
            EffectType::Float => "f",
            EffectType::Vec2 => "v2",
            EffectType::Vec3 => "v3",
            EffectType::Vec4 => "v4",
            EffectType::Bool => unreachable!(),
        };
        let name = format!("reel_{operation}_{suffix}");
        if !self.helpers.contains_key(&name) {
            let type_name = self.language.ty(ty);
            let mut source = match (self.language, operation) {
                (Language::GlslEs100, "remainder") => format!(
                    "{type_name} {name}({type_name} a, {type_name} b) {{\n    {type_name} q = a / b;\n    {type_name} t = sign(q) * floor(abs(q));\n    return a - b * t;\n}}\n"
                ),
                (Language::Wgsl, "remainder") => format!(
                    "fn {name}(a: {type_name}, b: {type_name}) -> {type_name} {{\n    let q = a / b;\n    let t = sign(q) * floor(abs(q));\n    return a - b * t;\n}}\n"
                ),
                _ => String::new(),
            };
            if operation == "smoothstep" {
                if ty == EffectType::Float {
                    source = match self.language {
                        Language::GlslEs100 => format!(
                            "float {name}(float a, float b, float x) {{\n    if (a == b) return step(a, x);\n    float t = clamp((x - a) / (b - a), 0.0, 1.0);\n    return t * t * (3.0 - 2.0 * t);\n}}\n"
                        ),
                        Language::Wgsl => format!(
                            "fn {name}(a: f32, b: f32, x: f32) -> f32 {{\n    if (a == b) {{ return step(a, x); }}\n    let t = clamp((x - a) / (b - a), 0.0, 1.0);\n    return t * t * (3.0 - 2.0 * t);\n}}\n"
                        ),
                    };
                } else {
                    let scalar = self.helper(operation, EffectType::Float)?;
                    let lanes = match ty {
                        EffectType::Vec2 => 2,
                        EffectType::Vec3 => 3,
                        _ => 4,
                    };
                    let values = ["x", "y", "z", "w"][..lanes]
                        .iter()
                        .map(|lane| format!("{scalar}(a.{lane}, b.{lane}, x.{lane})"))
                        .collect::<Vec<_>>();
                    let result = construct(self.language, ty, &values);
                    source = match self.language {
                        Language::GlslEs100 => format!(
                            "{type_name} {name}({type_name} a, {type_name} b, {type_name} x) {{ return {result}; }}\n"
                        ),
                        Language::Wgsl => format!(
                            "fn {name}(a: {type_name}, b: {type_name}, x: {type_name}) -> {type_name} {{ return {result}; }}\n"
                        ),
                    };
                }
            }
            self.helper_bytes += source.len();
            if self.body.len().saturating_add(self.helper_bytes) > self.max_bytes {
                return Err(GenerateError::SourceLimit { limit: self.limit });
            }
            self.helpers.insert(name.clone(), source);
        }
        Ok(name)
    }
}

fn dependencies(op: &EffectOp) -> Vec<EffectNodeId> {
    match *op {
        EffectOp::Constant(_) | EffectOp::Parameter(_) | EffectOp::Input(_) => vec![],
        EffectOp::SourceSample { uv } | EffectOp::TextureSample { uv, .. } => vec![uv],
        EffectOp::Vec2(ids) => ids.to_vec(),
        EffectOp::Vec3(ids) => ids.to_vec(),
        EffectOp::Vec4(ids) => ids.to_vec(),
        EffectOp::Extract { value, .. } | EffectOp::Unary { value, .. } => vec![value],
        EffectOp::Binary { left, right, .. } => vec![left, right],
        EffectOp::Ternary { a, b, c, .. } => vec![a, b, c],
        EffectOp::Select { .. } => unreachable!(),
    }
}

fn construct(language: Language, ty: EffectType, values: &[String]) -> String {
    format!("{}({})", language.ty(ty), values.join(", "))
}

fn constant(language: Language, value: EffectValue) -> String {
    let number = |bits| format!("{:e}", f64::from(f32::from_bits(bits)));
    match value {
        EffectValue::Bool(value) => value.to_string(),
        EffectValue::Float(bits) => number(bits),
        EffectValue::Vec2(bits) => construct(language, EffectType::Vec2, &bits.map(number)),
        EffectValue::Vec3(bits) => construct(language, EffectType::Vec3, &bits.map(number)),
        EffectValue::Vec4(bits) => construct(language, EffectType::Vec4, &bits.map(number)),
    }
}

#[cfg(test)]
#[path = "effect_tests.rs"]
mod tests;

fn unary_name(op: EffectUnary) -> &'static str {
    match op {
        EffectUnary::Abs => "abs",
        EffectUnary::Floor => "floor",
        EffectUnary::Ceil => "ceil",
        EffectUnary::Fract => "fract",
        EffectUnary::Sin => "sin",
        EffectUnary::Cos => "cos",
        EffectUnary::Tan => "tan",
        EffectUnary::Asin => "asin",
        EffectUnary::Acos => "acos",
        EffectUnary::Atan => "atan",
        EffectUnary::Sqrt => "sqrt",
        EffectUnary::Exp => "exp",
        EffectUnary::Log => "log",
        EffectUnary::Exp2 => "exp2",
        EffectUnary::Log2 => "log2",
        EffectUnary::Length => "length",
        EffectUnary::Normalize => "normalize",
        _ => unreachable!(),
    }
}

fn binary_name(op: EffectBinary) -> &'static str {
    match op {
        EffectBinary::Add => "+",
        EffectBinary::Subtract => "-",
        EffectBinary::Multiply => "*",
        EffectBinary::Divide => "/",
        EffectBinary::Pow => "pow",
        EffectBinary::Min => "min",
        EffectBinary::Max => "max",
        EffectBinary::Dot => "dot",
        EffectBinary::Distance => "distance",
        EffectBinary::Equal => "==",
        EffectBinary::NotEqual => "!=",
        EffectBinary::Less => "<",
        EffectBinary::LessEqual => "<=",
        EffectBinary::Greater => ">",
        EffectBinary::GreaterEqual => ">=",
        _ => unreachable!(),
    }
}
