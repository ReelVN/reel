struct Params { opacity: f32, mode: u32, progress: f32, unused: f32 };
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var mask: texture_2d<f32>;
@group(0) @binding(2) var linear_sampler: sampler;
@group(0) @binding(3) var<uniform> params: Params;
struct Varying { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vertex(@builtin(vertex_index) i: u32) -> Varying {
    let uv = vec2(f32((i << 1u) & 2u), f32(i & 2u));
    var out: Varying;
    out.pos = vec4(uv * vec2(2.0, -2.0) + vec2(-1.0, 1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}
@fragment fn fragment(v: Varying) -> @location(0) vec4<f32> {
    let s = textureSample(source, linear_sampler, v.uv);
    let m = textureSample(mask, linear_sampler, v.uv);
    if params.mode == 3u { return mix(s, m, params.progress); }
    var coverage = 1.0;
    if params.mode == 1u { coverage = clamp(m.a, 0.0, 1.0); }
    if params.mode == 2u { coverage = clamp(dot(m.rgb, vec3(0.2126, 0.7152, 0.0722)), 0.0, 1.0); }
    return s * coverage * params.opacity;
}
