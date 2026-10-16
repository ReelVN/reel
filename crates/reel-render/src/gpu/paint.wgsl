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
@fragment fn fragment(v: VertexOut) -> @location(0) vec4<f32> {
    let pixel = textureSample(image, image_sampler, v.uv) * v.color;
    if gradient.x.w == 0.0 { return pixel; }
    let position = vec3<f32>(v.position.xy, 1.0);
    let x = clamp(dot(gradient.x.xyz, position), 0.0, 1.0);
    let y = clamp(dot(gradient.y.xyz, position), 0.0, 1.0);
    return pixel * mix(mix(gradient.top_left, gradient.top_right, x),
        mix(gradient.bottom_left, gradient.bottom_right, x), y);
}
