use super::{Gpu, GpuError, Quality, mesh::Vertex, targets::Target};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use wgpu::util::DeviceExt;

/// Only the three existing format/sample pairs can occupy these instance slots.
/// No frame data or application-accounted allocations live here.
#[derive(Default)]
pub(super) struct PipelineSlots {
    slots: [Mutex<Option<Arc<Pipelines>>>; 3],
    // Publish the first failed constructor before another quality tier starts.
    construction: Mutex<()>,
    #[cfg(test)]
    pub(super) creations: Creations,
}
impl PipelineSlots {
    #[cfg(test)]
    pub(super) fn initialized_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.lock().unwrap().is_some())
            .count()
    }
    pub(super) fn get(&self, gpu: &Gpu, quality: Quality) -> Result<Arc<Pipelines>, GpuError> {
        gpu.check()?;
        let index = match (quality.format, quality.samples) {
            (wgpu::TextureFormat::Rgba16Float, 4) => 0,
            (wgpu::TextureFormat::Rgba16Float, 1) => 1,
            (wgpu::TextureFormat::Rgba8Unorm, 1) => 2,
            _ => return Err(GpuError::Device("unsupported pipeline quality".into())),
        };
        let mut slot = self.slots[index].lock().expect("pipeline slot poisoned");
        // Another caller may have initialized this slot while we were waiting.
        gpu.check()?;
        if let Some(pipelines) = &*slot {
            return Ok(pipelines.clone());
        }
        let _construction = self
            .construction
            .lock()
            .expect("pipeline construction poisoned");
        gpu.check()?;
        let scopes = [
            wgpu::ErrorFilter::Validation,
            wgpu::ErrorFilter::Internal,
            wgpu::ErrorFilter::OutOfMemory,
        ]
        .map(|filter| gpu.device.push_error_scope(filter));
        let pipelines = Arc::new(Pipelines::new(
            &gpu.device,
            quality,
            #[cfg(test)]
            &self.creations,
        ));
        // Pop every scope before returning, including on captured failure. The
        // native wgpu-core backend returns ready(scope.error); this does not wait
        // for queue completion. Never infer success from an uncaptured handler,
        // nor block this synchronous API on a WebGPU Promise.
        let mut pending = false;
        for scope in scopes.into_iter().rev() {
            let mut result = std::pin::pin!(scope.pop());
            match result
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
            {
                Poll::Ready(Some(error)) => gpu.capture_driver_error(error),
                Poll::Ready(None) => (),
                Poll::Pending => pending = true,
            }
        }
        gpu.check()?;
        if pending {
            return Err(GpuError::Device(
                "pipeline construction requires asynchronous validation on this backend".into(),
            ));
        }
        *slot = Some(pipelines.clone());
        Ok(pipelines)
    }
}

/// Test-only counts at the actual device creation calls, never lookup counts.
#[cfg(test)]
#[derive(Default)]
pub(super) struct Creations {
    pub(super) bind_layouts: std::sync::atomic::AtomicUsize,
    pub(super) shaders: std::sync::atomic::AtomicUsize,
    pub(super) pipeline_layouts: std::sync::atomic::AtomicUsize,
    pub(super) render_pipelines: std::sync::atomic::AtomicUsize,
    pub(super) samplers: std::sync::atomic::AtomicUsize,
    pub(super) invalid_shader: std::sync::atomic::AtomicBool,
}
#[cfg(test)]
impl Creations {
    pub(super) fn snapshot(&self) -> [usize; 5] {
        use std::sync::atomic::Ordering;
        [
            &self.bind_layouts,
            &self.shaders,
            &self.pipeline_layouts,
            &self.render_pipelines,
            &self.samplers,
        ]
        .map(|count| count.load(Ordering::Relaxed))
    }
}
pub struct CompositeParams {
    pub scissor: super::targets::Scissor,
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
    fn new(device: &wgpu::Device, quality: Quality, #[cfg(test)] counts: &Creations) -> Self {
        #[cfg(test)]
        counts
            .bind_layouts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let paint_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("paint"),
            entries: &[texture(0), sampler(1)],
        });
        #[cfg(test)]
        counts
            .bind_layouts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            #[cfg(test)]
            Some(counts),
        );
        let composite = pipeline(
            device,
            quality,
            &composite_layout,
            include_str!("composite.wgsl"),
            false,
            #[cfg(test)]
            Some(counts),
        );
        #[cfg(test)]
        counts
            .samplers
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            multiview_mask: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.composite);
        params.scissor.apply(&mut pass);
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
    #[cfg(test)] counts: Option<&Creations>,
) -> wgpu::RenderPipeline {
    #[cfg(test)]
    let code =
        if counts.is_some_and(|c| c.invalid_shader.load(std::sync::atomic::Ordering::Relaxed)) {
            "invalid WGSL for the construction failure regression"
        } else {
            code
        };
    #[cfg(test)]
    if let Some(counts) = counts {
        counts
            .shaders
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("reel linear shader"),
        source: wgpu::ShaderSource::Wgsl(code.into()),
    });
    #[cfg(test)]
    if let Some(counts) = counts {
        counts
            .pipeline_layouts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("reel"),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    let attributes = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4];
    let buffers = [Some(wgpu::VertexBufferLayout {
        array_stride: size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attributes,
    })];
    #[cfg(test)]
    if let Some(counts) = counts {
        counts
            .render_pipelines
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
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
        multiview_mask: None,
        cache: None,
    })
}
