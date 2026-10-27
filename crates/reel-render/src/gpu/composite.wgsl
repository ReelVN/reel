struct Params { opacity: f32, mode: u32, progress: f32, softness: f32, invert: f32, blend: u32, source_uv: vec4<f32>, mask_uv: vec4<f32>, backdrop_uv: vec4<f32> /*MASK_FIELDS*/ };
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var mask: texture_2d<f32>;
@group(0) @binding(2) var linear_sampler: sampler;
@group(0) @binding(3) var<uniform> params: Params;
@group(0) @binding(4) var backdrop: texture_2d<f32>;
struct Varying { @builtin(position) pos: vec4<f32>, @location(0) mask_uv: vec2<f32>, @location(1) source_uv: vec2<f32>, @location(2) backdrop_uv: vec2<f32> };
@vertex fn vertex(@builtin(vertex_index) i: u32) -> Varying {
    let uv = vec2(f32((i << 1u) & 2u), f32(i & 2u));
    var out: Varying;
    out.pos = vec4(uv * vec2(2.0, -2.0) + vec2(-1.0, 1.0), 0.0, 1.0);
    out.mask_uv = uv * params.mask_uv.xy + params.mask_uv.zw;
    out.backdrop_uv = uv * params.backdrop_uv.xy + params.backdrop_uv.zw;
    out.source_uv = uv * params.source_uv.xy + params.source_uv.zw;
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
/*MASK_COVERAGE*/
fn expression_coverage(position: vec2<f32>) -> f32 { return 1.0; }
/*END_MASK_COVERAGE*/
fn covered_source(v: Varying) -> vec4<f32> {
    var s = textureSample(source, linear_sampler, v.source_uv);
    if params.invert != 0.0 && s.a > 0.0 {
        let encoded = encoded_srgb(clamp(s.rgb / s.a, vec3(0.0), vec3(1.0)));
        s = vec4(linear_srgb(mix(encoded, vec3(1.0) - encoded, params.invert)) * s.a, s.a);
    }
    let m = textureSample(mask, linear_sampler, v.mask_uv);
    if params.mode == 3u { return mix(s, m, params.progress); }
    if params.mode == 4u {
        if params.progress <= 0.0 { return s; }
        if params.progress >= 1.0 { return m; }
        return mix(s, m, clamp(expression_coverage(v.pos.xy), 0.0, 1.0));
    }
    var coverage = 1.0;
    if params.mode == 1u { coverage = clamp(m.a, 0.0, 1.0); }
    if params.mode == 2u { coverage = clamp(dot(m.rgb, vec3(0.2126, 0.7152, 0.0722)), 0.0, 1.0); }
    return s * coverage * params.opacity * expression_coverage(v.pos.xy);
}

fn subtract(s: vec4<f32>, d: vec4<f32>) -> vec4<f32> {
    return vec4(max(d.rgb - s.rgb, vec3(0.0)), d.a);
}

// W3C separable blend functions on straight linear RGB; linear burn extends this set.
fn overlay(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    return mix(2.0 * b * s, 1.0 - 2.0 * (1.0 - b) * (1.0 - s), step(vec3(0.5), b));
}
fn hard_light(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    return mix(2.0 * b * s, 1.0 - 2.0 * (1.0 - b) * (1.0 - s), step(vec3(0.5), s));
}
// Keep the divisor finite at source one; backdrop zero takes priority.
fn color_dodge(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    return max(min(vec3(1.0), b / max(1.0 - s, step(vec3(1.0), s))), step(vec3(1.0), s)) * sign(b);
}
fn lighten(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    return max(b, s);
}
fn difference(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    return abs(b - s);
}
fn soft_curve(b: vec3<f32>) -> vec3<f32> {
    return mix(((16.0 * b - 12.0) * b + 4.0) * b, sqrt(b), step(vec3(0.25), b));
}
fn soft_light(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    return mix(b - (1.0 - 2.0 * s) * b * (1.0 - b), b + (2.0 * s - 1.0) * (soft_curve(b) - b), step(vec3(0.5), s));
}
fn linear_burn(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    return max(b + s - 1.0, vec3(0.0));
}
fn separable(s: vec4<f32>, d: vec4<f32>, mode: u32) -> vec4<f32> {
    // Zero-alpha colors contribute only the other input, without division.
    if s.a == 0.0 { return d; }
    if d.a == 0.0 { return s; }
    let b = clamp(d.rgb / d.a, vec3(0.0), vec3(1.0));
    let c = clamp(s.rgb / s.a, vec3(0.0), vec3(1.0));
    var blended = vec3(0.0);
    if mode == 5u { blended = overlay(b, c); }
    else if mode == 6u { blended = hard_light(b, c); }
    else if mode == 7u { blended = color_dodge(b, c); }
    else if mode == 8u { blended = lighten(b, c); }
    else if mode == 9u { blended = difference(b, c); }
    else if mode == 10u { blended = soft_light(b, c); }
    else { blended = linear_burn(b, c); }
    return vec4(s.rgb * (1.0 - d.a) + d.rgb * (1.0 - s.a) + s.a * d.a * blended, s.a + d.a * (1.0 - s.a));
}

@fragment fn source_over(v: Varying) -> @location(0) vec4<f32> {
    return textureSample(source, linear_sampler, v.source_uv) * params.opacity;
}

@fragment fn alpha_mask(v: Varying) -> @location(0) vec4<f32> {
    return textureSample(source, linear_sampler, v.source_uv) * clamp(textureSample(mask, linear_sampler, v.mask_uv).a, 0.0, 1.0) * params.opacity;
}

@fragment fn fragment(v: Varying) -> @location(0) vec4<f32> {
    let s = covered_source(v);
    if params.blend == 0u { return s; }
    let d = textureSample(backdrop, linear_sampler, v.backdrop_uv);
    if params.blend == 1u { return min(s + d, vec4(1.0)); }
    if params.blend == 4u { return subtract(s, d); }
    let alpha = s.a + d.a * (1.0 - s.a);
    if params.blend == 2u {
        return vec4(s.rgb * d.rgb + s.rgb * (1.0 - d.a) + d.rgb * (1.0 - s.a), alpha);
    }
    return vec4(s.rgb + d.rgb - s.rgb * d.rgb, alpha);
}

@fragment fn extended_blend(v: Varying) -> @location(0) vec4<f32> {
    return separable(covered_source(v), textureSample(backdrop, linear_sampler, v.backdrop_uv), params.blend);
}
