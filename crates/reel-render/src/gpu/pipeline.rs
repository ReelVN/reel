use super::{Quality, mesh::Vertex, targets::Target};
use wgpu::util::DeviceExt;
pub struct CompositeParams {
    pub opacity: f32,
    pub mode: u32,
    pub progress: f32,
}
pub struct Pipelines {
    pub paint: wgpu::RenderPipeline,
    pub composite: wgpu::RenderPipeline,
    pub paint_layout: wgpu::BindGroupLayout,
    pub composite_layout: wgpu::BindGroupLayout,
    pub sampler: wgpu::Sampler,
}
fn texture(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}
fn sampler(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}
impl Pipelines {
    pub fn new(device: &wgpu::Device, quality: Quality) -> Self {
        let paint_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("paint"),
            entries: &[texture(0), sampler(1)],
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("composite"),
            entries: &[
                texture(0),
                texture(1),
                sampler(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let paint = pipeline(
            device,
            quality,
            &paint_layout,
            include_str!("paint.wgsl"),
            true,
        );
        let composite = pipeline(
            device,
            quality,
            &composite_layout,
            include_str!("composite.wgsl"),
            false,
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear premultiplied"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            paint,
            composite,
            paint_layout,
            composite_layout,
            sampler,
        }
    }
    pub fn composite(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        target: &Target,
        source: &wgpu::TextureView,
        mask: &wgpu::TextureView,
        params: CompositeParams,
    ) {
        let words = [
            params.opacity.to_bits(),
            params.mode,
            params.progress.to_bits(),
            0,
        ];
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("composite params"),
            contents: bytemuck::cast_slice(&words),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("composite"),
            layout: &self.composite_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(mask),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("whole group composite"),
            color_attachments: &[Some(target.attachment(false))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.composite);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
    }
}
pub fn pipeline(
    device: &wgpu::Device,
    quality: Quality,
    layout: &wgpu::BindGroupLayout,
    code: &str,
    mesh: bool,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("reel linear shader"),
        source: wgpu::ShaderSource::Wgsl(code.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("reel"),
        bind_group_layouts: &[layout],
        push_constant_ranges: &[],
    });
    let attributes = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4];
    let buffers = [wgpu::VertexBufferLayout {
        array_stride: size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attributes,
    }];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("reel source over"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vertex"),
            compilation_options: Default::default(),
            buffers: if mesh { &buffers } else { &[] },
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fragment"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: quality.format,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState {
            count: quality.samples,
            ..Default::default()
        },
        multiview: None,
        cache: None,
    })
}
