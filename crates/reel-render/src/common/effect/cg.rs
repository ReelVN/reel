//! Native typed-IR emission for Sony Cg. Source textures and framebuffers share
//! top-left coordinates; register interpolation retains float32 TEXCOORD lanes.

use super::*;
use std::fmt::Write;

pub(super) fn compile(
    program: &EffectProgram,
    generated: GeneratedEffect,
    limit: usize,
) -> Result<EffectFragment, GenerateError> {
    let source_sampler = generated.samples_source || generated.output_type == EffectType::Float;
    let mut source =
        String::from("uniform float4 reel_context[9];\nuniform float4 reel_clip_rect;\n");
    source.push_str(&generated.declarations);
    if source_sampler {
        source.push_str("uniform sampler2D reel_source : TEXUNIT0;\n");
    }
    if generated.samples_source {
        source.push_str(r#"
float4 reel_source_sample(float2 uv) {
    float3 p = float3(clamp(uv, 0.0, 1.0), 1.0);
    float2 q = float2(dot(reel_context[2].xyz, p), dot(reel_context[3].xyz, p));
    float2 half_texel = 0.5 / reel_context[5].zw;
    q = clamp(q, reel_context[6].xy + half_texel, reel_context[6].xy + reel_context[6].zw - half_texel);
    return tex2D(reel_source, q);
}
"#);
    }
    texture_functions(program, source_sampler, &mut source, limit)?;
    source.push_str(&generated.function);
    source.push_str("\nfloat4 main(float2 v_uv : TEXCOORD0, float4 fragment_position : WPOS");
    if generated.output_type == EffectType::Float {
        source.push_str(", float2 reel_implicit_source_uv : TEXCOORD1");
    }
    source.push_str(") : COLOR {\n");
    source.push_str("if (any(fragment_position.xy < reel_clip_rect.xy) || any(fragment_position.xy >= reel_clip_rect.zw)) discard;\n");
    if generated.output_type == EffectType::Float {
        source.push_str(
            "float4 reel_implicit_source = tex2D(reel_source, reel_implicit_source_uv);\n",
        );
    }
    source.push_str(
        r#"
    float3 p = float3(v_uv * reel_context[5].xy, 1.0);
    float2 uv = float2(dot(reel_context[0].xyz, p), dot(reel_context[1].xyz, p));
    if (any(uv < float2(0.0)) || any(uv > float2(1.0))) discard;
    float2 position = reel_context[4].xy + uv * reel_context[4].zw;
"#,
    );
    if generated.output_type == EffectType::Float {
        source.push_str("float coverage = clamp(reel_effect(uv, position, reel_context[4].zw), 0.0, 1.0); float mode = reel_context[0].w; if (mode >= 2.0) { coverage = 1.0 - coverage; } if (mode == 1.0 || mode == 2.0) { return float4(coverage); } return reel_implicit_source * coverage;\n}\n");
    } else {
        source.push_str("return reel_effect(uv, position, reel_context[4].zw);\n}\n");
    }
    if source.len() > limit {
        return Err(GenerateError::SourceLimit { limit });
    }
    Ok(EffectFragment {
        source,
        parameters: generated.parameters,
        output_type: generated.output_type,
        source_sampler,
    })
}

fn texture_functions(
    program: &EffectProgram,
    source_sampler: bool,
    source: &mut String,
    limit: usize,
) -> Result<(), GenerateError> {
    let count = program.textures().len();
    if count == 0 {
        return Ok(());
    }
    if count > limit / 16 {
        return Err(GenerateError::SourceLimit { limit });
    }
    let mut channels = vec![0u8; count];
    for node in program.nodes() {
        if let EffectOp::TextureSample {
            binding, channel, ..
        } = node.op
        {
            channels[binding as usize] |= match channel {
                EffectTextureChannel::Color => 1,
                EffectTextureChannel::Alpha => 2,
                EffectTextureChannel::Gray => 4,
            };
        }
    }
    writeln!(source, "uniform float4 reel_texture_meta[{}];", count * 2).unwrap();
    if channels.iter().any(|usage| usage & 4 != 0) {
        source.push_str("float3 reel_texture_encode(float3 x){return lerp(1.055*pow(max(x,float3(0.0)),float3(1.0/2.4))-0.055,12.92*x,step(x,float3(0.0031308)));}\n");
    }
    for (binding, usage) in channels.into_iter().enumerate() {
        if usage == 0 {
            continue;
        }
        writeln!(
            source,
            "uniform sampler2D reel_texture_{binding} : TEXUNIT{};",
            binding + usize::from(source_sampler)
        )
        .unwrap();
        writeln!(source, "float4 reel_texture_{binding}_read(float2 uv){{float4 t=reel_texture_meta[{}];float2 h=reel_texture_meta[{}].xy;float4 s=tex2D(reel_texture_{binding},clamp(uv*t.xy,h,t.xy-h));if(t.w==1.0)s.a=1.0;if(t.w==2.0)s.rgb=min(s.rgb,float3(s.a));return s;}}", binding*2,binding*2+1).unwrap();
        if usage & 1 != 0 {
            writeln!(source,"float4 reel_texture_{binding}_color(float2 uv){{return reel_texture_{binding}_read(uv);}}").unwrap();
        }
        if usage & 2 != 0 {
            writeln!(source,"float reel_texture_{binding}_alpha(float2 uv){{return reel_texture_{binding}_read(uv).a;}}").unwrap();
        }
        if usage & 4 != 0 {
            writeln!(source,"float reel_texture_{binding}_gray(float2 uv){{float4 s=reel_texture_{binding}_read(uv);float3 rgb=s.a>0.0?clamp(s.rgb/s.a,0.0,1.0):float3(0.0);if(reel_texture_meta[{}].z==0.0)rgb=reel_texture_encode(rgb);return dot(rgb,float3(0.2126,0.7152,0.0722));}}",binding*2).unwrap();
        }
        if source.len() > limit {
            return Err(GenerateError::SourceLimit { limit });
        }
    }
    Ok(())
}
