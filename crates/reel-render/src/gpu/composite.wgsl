struct Params { opacity: f32, mode: u32, progress: f32, softness: f32, invert: f32 };
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
fn encoded_srgb(linear: vec3<f32>) -> vec3<f32> {
    return select(
        1.055 * pow(linear, vec3(1.0 / 2.4)) - 0.055,
        12.92 * linear,
        linear <= vec3(0.0031308),
    );
}
fn linear_srgb(encoded: vec3<f32>) -> vec3<f32> {
    return select(
        pow((encoded + 0.055) / 1.055, vec3(2.4)),
        encoded / 12.92,
        encoded <= vec3(0.04045),
    );
}
@fragment fn fragment(v: Varying) -> @location(0) vec4<f32> {
    var s = textureSample(source, linear_sampler, v.uv);
    if params.invert != 0.0 && s.a > 0.0 {
        let encoded = encoded_srgb(clamp(s.rgb / s.a, vec3(0.0), vec3(1.0)));
        s = vec4(linear_srgb(mix(encoded, vec3(1.0) - encoded, params.invert)) * s.a, s.a);
    }
    let m = textureSample(mask, linear_sampler, v.uv);
    if params.mode == 3u { return mix(s, m, params.progress); }
    var coverage = 1.0;
    if params.mode == 1u { coverage = clamp(m.a, 0.0, 1.0); }
    if params.mode == 2u { coverage = clamp(dot(m.rgb, vec3(0.2126, 0.7152, 0.0722)), 0.0, 1.0); }
    if params.mode == 4u || params.mode == 5u {
        if params.progress <= 0.0 { return vec4(0.0); }
        if params.progress >= 1.0 { return s * params.opacity; }
        if m.a <= 0.0 { return vec4(0.0); }
        // Image resources use linear premultiplied pixels. Thresholds are authored
        // against the encoded image gray, so undo that conversion only here.
        let encoded = encoded_srgb(clamp(m.rgb / m.a, vec3(0.0), vec3(1.0)));
        var gray = dot(encoded, vec3(0.2126, 0.7152, 0.0722));
        if params.mode == 5u { gray = 1.0 - gray; }
        if params.softness == 0.0 {
            coverage = m.a * select(0.0, 1.0, gray <= params.progress);
        } else {
            coverage = m.a * clamp(
                (params.progress * (1.0 + params.softness) - gray) / params.softness,
                0.0, 1.0,
            );
        }
    }
    return s * coverage * params.opacity;
}
