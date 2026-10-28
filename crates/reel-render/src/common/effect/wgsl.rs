//! WGSL entry points and static image helpers for the shared typed effect graph.
use super::*;
use std::fmt::Write;

pub(super) fn compile(
    program: &EffectProgram,
    generated: GeneratedEffect,
    tail: OutputTail,
    max_source_bytes: usize,
) -> Result<EffectFragment, GenerateError> {
    let source_sampler = generated.samples_source || generated.output_type == EffectType::Float;
    let masked = tail == OutputTail::AlphaCoverage;
    let mut source = String::new();
    if masked {
        writeln!(
            source,
            "@group(0) @binding({}) var reel_composite_mask: texture_2d<f32>;",
            5 + program.textures().len()
        )
        .unwrap();
    }
    source.push_str(
        r#"
struct ReelEffectContext { slots: array<vec4<f32>, 10> }
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
    source.push_str(
        r#"
fn reel_output(color: vec4<f32>, coverage: f32) -> vec4<f32> {
    let mode = reel_context.slots[1].w;
    if mode == 0.0 { return color; }
    var stored = color;
    if mode == 1.0 { stored = floor(clamp(color, vec4(0.0), vec4(1.0)) * 255.0 + 0.5) / 255.0; }
    if mode == 2.0 { stored = quantizeToF16(color); }
    return stored * coverage * reel_context.slots[2].w;
}
"#,
    );
    if masked {
        source = source.replace("return stored * coverage * reel_context.slots[2].w;", "if reel_context.slots[3].w != 0.0 { stored *= reel_context.slots[2].w; if mode == 1.0 { stored = floor(clamp(stored, vec4(0.0), vec4(1.0)) * 255.0 + 0.5) / 255.0; } if mode == 2.0 { stored = quantizeToF16(stored); } return stored * coverage; } return stored * coverage * reel_context.slots[2].w;");
    }
    if generated.output_type == EffectType::Float {
        source.push_str("struct ReelEffectVarying { @builtin(position) position: vec4<f32>, @location(0) implicit_source_uv: vec2<f32> }\n");
    } else if masked {
        source.push_str("struct ReelEffectVarying { @builtin(position) position: vec4<f32>, @location(1) mask_uv: vec2<f32> }\n");
    } else {
        source.push_str("struct ReelEffectVarying { @builtin(position) position: vec4<f32> }\n");
    }
    source.push_str(
        r#"
@vertex fn vertex(@builtin(vertex_index) i: u32) -> ReelEffectVarying {
    let uv = vec2(f32((i << 1u) & 2u), f32(i & 2u));
    var out: ReelEffectVarying;
    out.position = vec4(uv * vec2(2.0, -2.0) + vec2(-1.0, 1.0), 0.0, 1.0);
"#,
    );
    if masked {
        source
            .push_str("out.mask_uv = uv * reel_context.slots[9].xy + reel_context.slots[9].zw;\n");
    }
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
        max_source_bytes.saturating_sub(source.len() + generated.function.len()),
    )?);
    source.push_str(&generated.function);
    source.push_str("\n@fragment fn fragment(v: ReelEffectVarying) -> @location(0) vec4<f32> {\n");
    if masked {
        source.push_str("let reel_tail_coverage = clamp(textureSampleLevel(reel_composite_mask,reel_sampler,v.mask_uv,0.0).a,0.0,1.0);\n");
    } else {
        source.push_str("let reel_tail_coverage = 1.0;\n");
    }
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
        source.push_str("var coverage = clamp(reel_effect(uv, position, reel_context.slots[4].zw), 0.0, 1.0); let mode = reel_context.slots[0].w; if mode >= 2.0 { coverage = 1.0 - coverage; } if mode == 1.0 || mode == 2.0 { return reel_output(vec4(coverage), reel_tail_coverage); } return reel_output(reel_implicit_source * coverage, reel_tail_coverage);\n}\n");
    } else {
        source.push_str(
            "return reel_output(reel_effect(uv, position, reel_context.slots[4].zw), reel_tail_coverage);\n}\n",
        );
    }
    if !source_sampler {
        source = source.replace(
            "@group(0) @binding(0) var reel_source: texture_2d<f32>;\n",
            "",
        );
        let start = source.find("fn reel_source_sample(").unwrap();
        let end = source[start..].find("\n}\n").unwrap() + start + 3;
        source.replace_range(start..end, "");
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

fn texture_functions(program: &EffectProgram, limit: usize) -> Result<String, GenerateError> {
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
    writeln!(code, "struct ReelTextureMetadata {{ values: array<vec4<f32>,{}> }}\n@group(0) @binding(4) var<uniform> reel_texture_meta: ReelTextureMetadata;", count * 2).unwrap();
    if channel_uses.iter().any(|usage| usage & 4 != 0) {
        code.push_str("fn reel_texture_encode(x:vec3<f32>)->vec3<f32>{return select(1.055*pow(max(x,vec3(0.0)),vec3(1.0/2.4))-0.055,12.92*x,x<=vec3(0.0031308));}\n");
    }
    for (binding, &channels) in channel_uses.iter().enumerate() {
        if channels == 0 {
            continue;
        }
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
        if code.len() > limit {
            return Err(GenerateError::SourceLimit { limit });
        }
    }
    if code.len() > limit {
        return Err(GenerateError::SourceLimit { limit });
    }
    Ok(code)
}
