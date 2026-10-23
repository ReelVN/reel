struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) source_window: vec4<f32>,
};
@vertex fn vertex(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>, @location(3) source_window: vec4<f32>) -> VertexOut {
    var out: VertexOut;
    out.position = vec4(position, 0.0, 1.0);
    out.uv = uv;
    out.color = color;
    out.source_window = source_window;
    return out;
}
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
struct ImageAppearance {
    top_left: vec4<f32>,
    top_right: vec4<f32>,
    bottom_right: vec4<f32>,
    bottom_left: vec4<f32>,
    x: vec4<f32>,
    y: vec4<f32>,
    gain_saturation: vec4<f32>,
    offset_enabled: vec4<f32>,
}
@group(0) @binding(2) var<uniform> appearance: ImageAppearance;
// Stops interpolate in their declared space before entering linear composition.
fn linear_paint(position: vec2<f32>) -> vec4<f32> {
    var t = dot(appearance.x.xyz, vec3<f32>(position, 1.0));
    if appearance.y.x == 1.0 {
        t = fract(t);
    } else if appearance.y.x == 2.0 {
        t = 1.0 - abs((t - 2.0 * floor(t * 0.5)) - 1.0);
    } else {
        t = clamp(t, 0.0, 1.0);
    }
    let start = appearance.gain_saturation.x;
    let end = appearance.gain_saturation.y;
    var u: f32;
    if start == end {
        u = select(0.0, 1.0, t >= end);
    } else {
        u = clamp((t - start) / (end - start), 0.0, 1.0);
    }
    let color = mix(appearance.top_left, appearance.top_right, u);
    if color.a == 0.0 { return vec4<f32>(0.0); }
    var rgb = color.rgb;
    if appearance.y.z != 0.0 { rgb /= color.a; }
    if appearance.y.y != 0.0 {
        let magnitude = abs(rgb);
        rgb = sign(rgb) * select(pow((magnitude + 0.055) / 1.055, vec3<f32>(2.4)),
            magnitude / 12.92, magnitude <= vec3<f32>(0.04045));
    }
    return vec4<f32>(rgb * color.a, color.a);
}
fn tint(v: VertexOut, pixel: vec4<f32>) -> vec4<f32> {
    if appearance.x.w == 0.0 { return pixel; }
    if appearance.x.w == 2.0 { return pixel * linear_paint(v.position.xy); }
    var tinted = pixel;
    if appearance.y.w != 0.0 {
        let position = vec3<f32>(v.position.xy, 1.0);
        let x = clamp(dot(appearance.x.xyz, position), 0.0, 1.0);
        let y = clamp(dot(appearance.y.xyz, position), 0.0, 1.0);
        tinted *= mix(mix(appearance.top_left, appearance.top_right, x),
            mix(appearance.bottom_left, appearance.bottom_right, x), y);
    }
    if appearance.offset_enabled.w == 0.0 { return tinted; }
    if tinted.a == 0.0 { return tinted; }
    let linear = max(tinted.rgb / tinted.a, vec3<f32>(0.0));
    let srgb = select(1.055 * pow(linear, vec3<f32>(1.0 / 2.4)) - 0.055,
        12.92 * linear, linear <= vec3<f32>(0.0031308));
    let gray = dot(srgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let saturated = vec3<f32>(gray) + appearance.gain_saturation.w * (srgb - vec3<f32>(gray));
    let adjusted = clamp(saturated * appearance.gain_saturation.xyz + appearance.offset_enabled.xyz,
        vec3<f32>(0.0), vec3<f32>(1.0));
    let adjusted_linear = select(pow((adjusted + 0.055) / 1.055, vec3<f32>(2.4)),
        adjusted / 12.92, adjusted <= vec3<f32>(0.04045));
    return vec4<f32>(adjusted_linear * tinted.a, tinted.a);
}
// Clamp at sampling time, leaving interpolated UVs and 1:1 pixel centers unchanged.
// Chroma planes use the same normalized window and their own half-texel size.
fn source_uv(v: VertexOut, dimensions: vec2<u32>) -> vec2<f32> {
    let low = v.source_window.xy;
    let span = abs(v.source_window.zw);
    let high = low + span;
    if any(span <= vec2<f32>(0.0)) { return v.uv; }
    var uv = v.uv;
    if any(v.source_window.zw < vec2<f32>(0.0)) {
        let wrapped = low + fract((uv - low) / span) * span;
        uv = select(uv, wrapped, v.source_window.zw < vec2<f32>(0.0));
    }
    let half_texel = 0.5 / vec2<f32>(dimensions);
    let inset = min(half_texel, span * 0.5);
    // Regional repetition is already resolved above. Even a repeating sampler
    // must pad the physical texture when the authored region crosses its edge.
    return clamp(clamp(uv, low + inset, high - inset), half_texel, vec2<f32>(1.0) - half_texel);
}
@fragment fn fragment(v: VertexOut) -> @location(0) vec4<f32> {
    return tint(v, textureSample(image, image_sampler, source_uv(v, textureDimensions(image))) * v.color);
}
@group(0) @binding(3) var chroma_u: texture_2d<f32>;
@group(0) @binding(4) var chroma_v: texture_2d<f32>;
struct VideoColor {
    r: vec4<f32>, g: vec4<f32>, b: vec4<f32>, transfer: vec4<f32>,
}
@group(0) @binding(5) var<uniform> video_color: VideoColor;
@fragment fn fragment_yuv(v: VertexOut) -> @location(0) vec4<f32> {
    let yuv = vec4<f32>(textureSample(image, image_sampler, source_uv(v, textureDimensions(image))).r,
        textureSample(chroma_u, image_sampler, source_uv(v, textureDimensions(chroma_u))).r,
        textureSample(chroma_v, image_sampler, source_uv(v, textureDimensions(chroma_v))).r, 1.0);
    let rgb = clamp(vec3<f32>(dot(video_color.r, yuv), dot(video_color.g, yuv), dot(video_color.b, yuv)), vec3<f32>(0.0), vec3<f32>(1.0));
    var linear: vec3<f32>;
    if video_color.transfer.x == 1.0 {
        linear = select(pow((rgb + 0.055) / 1.055, vec3<f32>(2.4)), rgb / 12.92, rgb <= vec3<f32>(0.04045));
    } else {
        linear = select(pow((rgb + 0.099) / 1.099, vec3<f32>(1.0 / 0.45)), rgb / 4.5, rgb < vec3<f32>(0.081));
    }
    return tint(v, vec4<f32>(linear, 1.0) * v.color);
}
