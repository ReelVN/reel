//! Final linear-to-sRGB surface pass. Native surface acquisition remains in player.
use super::{GpuError, Prepared, Quality, pipeline};
pub struct SurfacePass {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}
impl SurfacePass {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Result<Self, GpuError> {
        if !format.is_srgb() {
            return Err(GpuError::Device("surface requires an sRGB format".into()));
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("surface output"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let shader = r#"
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var linear_sampler: sampler;
struct Varying { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vertex(@builtin(vertex_index) i: u32) -> Varying {
    let uv = vec2(f32((i << 1u) & 2u), f32(i & 2u));
    var v: Varying; v.pos = vec4(uv * vec2(2.0, -2.0) + vec2(-1.0, 1.0), 0.0, 1.0); v.uv = uv; return v;
}
@fragment fn fragment(v: Varying) -> @location(0) vec4<f32> {
    let c = textureSample(source, linear_sampler, v.uv);
    return vec4(c.rgb, 1.0);
}
"#;
        let pipeline = pipeline::pipeline(
            device,
            Quality {
                format,
                samples: 1,
                degraded: false,
            },
            &layout,
            shader,
            false,
            #[cfg(test)]
            None,
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Ok(Self {
            pipeline,
            layout,
            sampler,
        })
    }
    pub fn encode(
        &self,
        device: &wgpu::Device,
        prepared: &mut Prepared,
        surface: &wgpu::TextureView,
    ) {
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("surface source"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&prepared.output),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let mut pass = prepared
            .encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("surface encode once"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: surface,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
    }
}
