// Only construction-proven [0,1] colors and UNORM textures select this module.
// Color conversion/interpolation and image multiplication have <=4/4096
// absolute error; positions, texture coordinates and texture filtering stay f32.
enable f16;
alias PaintVertexColor = vec4<f16>;
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: PaintVertexColor,
    @location(2) @interpolate(flat) source_window: vec4<f32>,
};
@vertex fn vertex(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>, @location(3) source_window: vec4<f32>) -> VertexOut {
    var out: VertexOut;
    out.position = vec4(position, 0.0, 1.0);
    out.uv = uv;
    out.color = vec4<f16>(color);
    out.source_window = source_window;
    return out;
}
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
@fragment fn fragment_solid(v: VertexOut) -> @location(0) vec4<f32> {
    return vec4<f32>(v.color);
}
@fragment fn fragment_direct(v: VertexOut) -> @location(0) vec4<f32> {
    return vec4<f32>(vec4<f16>(textureSample(image, image_sampler, v.uv)) * v.color);
}
@fragment fn fragment_direct_bc1(v: VertexOut) -> @location(0) vec4<f32> {
    let pixel = vec4<f16>(textureSample(image, image_sampler, v.uv));
    return vec4<f32>(vec4<f16>(pixel.rgb, 1.0h) * v.color);
}
@fragment fn fragment_direct_bc3(v: VertexOut) -> @location(0) vec4<f32> {
    let pixel = vec4<f16>(textureSample(image, image_sampler, v.uv));
    return vec4<f32>(vec4<f16>(min(pixel.rgb, vec3<f16>(pixel.a)), pixel.a) * v.color);
}
fn regional_uv(v: VertexOut) -> vec2<f32> {
    let half_texel = 0.5 / vec2<f32>(textureDimensions(image));
    let inset = min(half_texel, v.source_window.zw * 0.5);
    return clamp(clamp(v.uv, v.source_window.xy + inset,
                       v.source_window.xy + v.source_window.zw - inset),
                 half_texel, vec2<f32>(1.0) - half_texel);
}
@fragment fn fragment_regional(v: VertexOut) -> @location(0) vec4<f32> {
    return vec4<f32>(vec4<f16>(textureSample(image, image_sampler, regional_uv(v))) * v.color);
}
@fragment fn fragment_regional_bc1(v: VertexOut) -> @location(0) vec4<f32> {
    let pixel = vec4<f16>(textureSample(image, image_sampler, regional_uv(v)));
    return vec4<f32>(vec4<f16>(pixel.rgb, 1.0h) * v.color);
}
@fragment fn fragment_regional_bc3(v: VertexOut) -> @location(0) vec4<f32> {
    let pixel = vec4<f16>(textureSample(image, image_sampler, regional_uv(v)));
    return vec4<f32>(vec4<f16>(min(pixel.rgb, vec3<f16>(pixel.a)), pixel.a) * v.color);
}
