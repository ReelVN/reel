// Only construction-proven [0,1] colors and UNORM textures select this module.
// Color conversion/interpolation and image multiplication have <=4/4096
// absolute error; positions, texture coordinates and texture filtering stay f32.
enable f16;
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f16>,
};
@vertex fn vertex(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>, @location(3) source_window: vec4<f32>) -> VertexOut {
    var out: VertexOut;
    out.position = vec4(position, 0.0, 1.0);
    out.uv = uv;
    out.color = vec4<f16>(color);
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
