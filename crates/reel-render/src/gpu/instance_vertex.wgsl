@vertex fn instance_vertex(
    @location(0) corner: f32, @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>, @location(3) source_window: vec4<f32>,
    @location(4) positions01: vec4<f32>, @location(5) positions23: vec4<f32>,
) -> VertexOut {
    var position = positions01.xy;
    if corner == 1.0 { position = positions01.zw; }
    else if corner == 2.0 { position = positions23.xy; }
    else if corner == 3.0 { position = positions23.zw; }
    var out: VertexOut;
    out.position = vec4<f32>(position, 0.0, 1.0);
    out.uv = uv;
    out.color = PaintVertexColor(color);
    out.source_window = source_window;
    return out;
}
