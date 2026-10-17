struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};
@vertex fn vertex(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>) -> VertexOut {
    var out: VertexOut;
    out.position = vec4(position, 0.0, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
struct ImageGradient {
    top_left: vec4<f32>,
    top_right: vec4<f32>,
    bottom_right: vec4<f32>,
    bottom_left: vec4<f32>,
    x: vec4<f32>,
    y: vec4<f32>,
}
@group(0) @binding(2) var<uniform> gradient: ImageGradient;
fn tint(v: VertexOut, pixel: vec4<f32>) -> vec4<f32> {
    if gradient.x.w == 0.0 { return pixel; }
    let position = vec3<f32>(v.position.xy, 1.0);
    let x = clamp(dot(gradient.x.xyz, position), 0.0, 1.0);
    let y = clamp(dot(gradient.y.xyz, position), 0.0, 1.0);
    return pixel * mix(mix(gradient.top_left, gradient.top_right, x),
        mix(gradient.bottom_left, gradient.bottom_right, x), y);
}
@fragment fn fragment(v: VertexOut) -> @location(0) vec4<f32> {
    return tint(v, textureSample(image, image_sampler, v.uv) * v.color);
}
@group(0) @binding(3) var chroma_u: texture_2d<f32>;
@group(0) @binding(4) var chroma_v: texture_2d<f32>;
struct VideoColor {
    r: vec4<f32>, g: vec4<f32>, b: vec4<f32>, transfer: vec4<f32>,
}
@group(0) @binding(5) var<uniform> video_color: VideoColor;
@fragment fn fragment_yuv(v: VertexOut) -> @location(0) vec4<f32> {
    let yuv = vec4<f32>(textureSample(image, image_sampler, v.uv).r,
        textureSample(chroma_u, image_sampler, v.uv).r,
        textureSample(chroma_v, image_sampler, v.uv).r, 1.0);
    let rgb = clamp(vec3<f32>(dot(video_color.r, yuv), dot(video_color.g, yuv), dot(video_color.b, yuv)), vec3<f32>(0.0), vec3<f32>(1.0));
    var linear: vec3<f32>;
    if video_color.transfer.x == 1.0 {
        linear = select(pow((rgb + 0.055) / 1.055, vec3<f32>(2.4)), rgb / 12.92, rgb <= vec3<f32>(0.04045));
    } else {
        linear = select(pow((rgb + 0.099) / 1.099, vec3<f32>(1.0 / 0.45)), rgb / 4.5, rgb < vec3<f32>(0.081));
    }
    return tint(v, vec4<f32>(linear, 1.0) * v.color);
}
