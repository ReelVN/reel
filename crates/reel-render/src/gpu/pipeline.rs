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
    pub(super) fn prune_masks(&self) {
        for slot in &self.slots {
            if let Some(pipelines) = slot.lock().unwrap().as_ref() {
                pipelines.masks.prune();
                pipelines.effects.prune();
            }
        }
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
        let pipelines = validated(gpu, || {
            Arc::new(Pipelines::new(
                &gpu.device,
                quality,
                #[cfg(test)]
                &self.creations,
            ))
        })?;
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
#[derive(Clone, Copy)]
pub(super) enum CompositeOperation {
    SourceOver,
    AlphaMask,
    LuminanceMask,
    Crossfade(f32),
    Transition(f32),
    MaskedTransition(f32),
}

impl CompositeOperation {
    pub(super) fn replaces(self) -> bool {
        matches!(self, Self::Transition(_) | Self::MaskedTransition(_))
    }
}

pub(super) struct CompositeParams {
    pub clear: bool,
    pub scissor: super::targets::Scissor,
    pub opacity: f32,
    pub invert: f32,
    pub operation: CompositeOperation,
    pub blend: u32,
    /// Integer parent pixels preceding the source's top-left storage texel.
    pub source_origin: [i32; 2],
}
pub(super) const COMPOSITE_WORDS: usize = 12;
pub(super) const COMPOSITE_BYTES: u64 = (COMPOSITE_WORDS * size_of::<u32>()) as u64;
impl CompositeParams {
    pub(super) fn uniform_words(
        &self,
        target: &Target,
        source: &wgpu::TextureView,
    ) -> [u32; COMPOSITE_WORDS] {
        let size = source.texture().size();
        let uv = crate::common::source_view::uv_mapping(
            target.size,
            [size.width, size.height],
            self.source_origin,
        )
        .map(f32::to_bits);
        let (mode, progress, softness) = operation_words(self.operation);
        [
            self.opacity.to_bits(),
            mode,
            progress.to_bits(),
            softness.to_bits(),
            self.invert.to_bits(),
            self.blend,
            0,
            0,
            uv[0],
            uv[1],
            uv[2],
            uv[3],
        ]
    }
}
pub(super) struct CompositeInputs<'a> {
    pub source: &'a wgpu::TextureView,
    pub mask: &'a wgpu::TextureView,
    pub backdrop: &'a wgpu::TextureView,
}
pub struct Pipelines {
    pub paint: wgpu::RenderPipeline,
    pub coverage: wgpu::RenderPipeline,
    #[cfg(test)]
    pub coverage_draws: std::sync::atomic::AtomicUsize,
    pub direct: [wgpu::RenderPipeline; 8],
    #[cfg(test)]
    pub direct_draws: [std::sync::atomic::AtomicUsize; 8],
    pub opaque: [wgpu::RenderPipeline; 3],
    #[cfg(test)]
    pub opaque_draws: [std::sync::atomic::AtomicUsize; 3],
    pub yuv: wgpu::RenderPipeline,
    pub yuv_layout: wgpu::BindGroupLayout,
    pub composite: wgpu::RenderPipeline,
    pub source_over: wgpu::RenderPipeline,
    pub alpha_mask: wgpu::RenderPipeline,
    pub composite_blend: wgpu::RenderPipeline,
    pub paint_layout: wgpu::BindGroupLayout,
    pub composite_layout: wgpu::BindGroupLayout,
    pub samplers: [wgpu::Sampler; 4],
    pub nearest_samplers: [wgpu::Sampler; 4],
    pub plain_appearance: wgpu::Buffer,
    pub block_appearance: [wgpu::Buffer; 2],
    pub masks: super::mask::Cache,
    pub effects: super::effect::Cache,
}
pub(super) fn texture(binding: u32) -> wgpu::BindGroupLayoutEntry {
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
pub(super) fn sampler(binding: u32) -> wgpu::BindGroupLayoutEntry {
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
            entries: &[
                texture(0),
                sampler(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
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
                texture(4),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
        let coverage = pipeline_entry(
            device,
            quality,
            &paint_layout,
            include_str!("paint.wgsl"),
            true,
            FragmentOutput {
                entry: "fragment_coverage",
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            },
            #[cfg(test)]
            Some(counts),
        );
        let direct = [
            "fragment_direct",
            "fragment_direct_bc1",
            "fragment_direct_bc3",
            "fragment_regional",
            "fragment_regional_bc1",
            "fragment_regional_bc3",
            "fragment_direct_coverage",
            "fragment_solid",
        ]
        .map(|entry| {
            pipeline_entry(
                device,
                quality,
                &paint_layout,
                include_str!("paint.wgsl"),
                true,
                FragmentOutput {
                    entry,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                },
                #[cfg(test)]
                Some(counts),
            )
        });
        let opaque = [
            "fragment_direct_bc1",
            "fragment_regional_bc1",
            "fragment_solid",
        ]
        .map(|entry| {
            pipeline_entry(
                device,
                quality,
                &paint_layout,
                include_str!("paint.wgsl"),
                true,
                FragmentOutput { entry, blend: None },
                #[cfg(test)]
                Some(counts),
            )
        });
        #[cfg(test)]
        counts
            .bind_layouts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let uniform = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let yuv_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("planar video"),
            entries: &[
                texture(0),
                sampler(1),
                uniform(2),
                texture(3),
                texture(4),
                uniform(5),
            ],
        });
        let yuv = pipeline_entry(
            device,
            quality,
            &yuv_layout,
            include_str!("paint.wgsl"),
            true,
            FragmentOutput {
                entry: "fragment_yuv",
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            },
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
        let composite_blend = pipeline_entry(
            device,
            quality,
            &composite_layout,
            include_str!("composite.wgsl"),
            false,
            FragmentOutput {
                entry: "fragment",
                blend: None,
            },
            #[cfg(test)]
            Some(counts),
        );
        let source_over = pipeline_entry(
            device,
            quality,
            &composite_layout,
            include_str!("composite.wgsl"),
            false,
            FragmentOutput {
                entry: "source_over",
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            },
            #[cfg(test)]
            Some(counts),
        );
        let alpha_mask = pipeline_entry(
            device,
            quality,
            &composite_layout,
            include_str!("composite.wgsl"),
            false,
            FragmentOutput {
                entry: "alpha_mask",
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            },
            #[cfg(test)]
            Some(counts),
        );
        #[cfg(test)]
        counts
            .samplers
            .fetch_add(8, std::sync::atomic::Ordering::Relaxed);
        let create_samplers = |filter| {
            std::array::from_fn(|index| {
                device.create_sampler(&wgpu::SamplerDescriptor {
                    label: Some("premultiplied image sampling"),
                    mag_filter: filter,
                    min_filter: filter,
                    address_mode_u: if index & 1 != 0 {
                        wgpu::AddressMode::Repeat
                    } else {
                        wgpu::AddressMode::ClampToEdge
                    },
                    address_mode_v: if index & 2 != 0 {
                        wgpu::AddressMode::Repeat
                    } else {
                        wgpu::AddressMode::ClampToEdge
                    },
                    ..Default::default()
                })
            })
        };
        let samplers = create_samplers(wgpu::FilterMode::Linear);
        let nearest_samplers = create_samplers(wgpu::FilterMode::Nearest);
        let plain_appearance = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("plain image appearance"),
            contents: &[0u8; 144],
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let block_appearance = std::array::from_fn(|index| {
            let mut values = [[0f32; 4]; 9];
            values[8][2] = (index + 1) as f32;
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("fixed block texture appearance"),
                contents: bytemuck::cast_slice(&values),
                usage: wgpu::BufferUsages::UNIFORM,
            })
        });
        Self {
            paint,
            coverage,
            #[cfg(test)]
            coverage_draws: Default::default(),
            direct,
            #[cfg(test)]
            direct_draws: Default::default(),
            opaque,
            #[cfg(test)]
            opaque_draws: Default::default(),
            yuv,
            yuv_layout,
            composite,
            source_over,
            alpha_mask,
            composite_blend,
            paint_layout,
            composite_layout,
            samplers,
            nearest_samplers,
            plain_appearance,
            block_appearance,
            masks: super::mask::Cache::default(),
            effects: super::effect::Cache::default(),
        }
    }
    fn composite_group(
        &self,
        device: &wgpu::Device,
        target: &Target,
        inputs: CompositeInputs<'_>,
        params: &CompositeParams,
    ) -> wgpu::BindGroup {
        let CompositeInputs {
            source,
            mask,
            backdrop,
        } = inputs;
        let words = params.uniform_words(target, source);
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("composite params"),
            contents: bytemuck::cast_slice(&words),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        device.create_bind_group(&wgpu::BindGroupDescriptor {
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
                    resource: wgpu::BindingResource::Sampler(&self.samplers[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(backdrop),
                },
            ],
        })
    }
    fn composite_pipeline(&self, params: &CompositeParams) -> &wgpu::RenderPipeline {
        let mode = operation_words(params.operation).0;
        if mode == 0 && params.invert == 0. && params.blend == 0 {
            &self.source_over
        } else if mode == 1 && params.invert == 0. && params.blend == 0 {
            &self.alpha_mask
        } else if params.blend == 0 && !params.operation.replaces() {
            &self.composite
        } else {
            &self.composite_blend
        }
    }
    pub(super) fn composite(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        target: &Target,
        inputs: CompositeInputs<'_>,
        params: CompositeParams,
    ) {
        let group = self.composite_group(device, target, inputs, &params);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("whole group composite"),
            color_attachments: &[Some(target.attachment(params.clear))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            multiview_mask: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(self.composite_pipeline(&params));
        params.scissor.apply(&mut pass);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
    }
    /// Independent sources return to their parent in one ordered render pass.
    pub(super) fn composite_batch<'a>(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        target: &Target,
        clear: bool,
        inputs: impl Iterator<Item = (CompositeInputs<'a>, CompositeParams)>,
    ) {
        let groups: smallvec::SmallVec<[(wgpu::BindGroup, CompositeParams); 8]> = inputs
            .filter(|(_, params)| !params.scissor.empty())
            .map(|(inputs, params)| {
                (
                    self.composite_group(device, target, inputs, &params),
                    params,
                )
            })
            .collect();
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ordered sibling composites"),
            color_attachments: &[Some(target.attachment(clear))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            multiview_mask: None,
            occlusion_query_set: None,
        });
        for (group, params) in &groups {
            pass.set_pipeline(self.composite_pipeline(params));
            params.scissor.apply(&mut pass);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        }
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
    pipeline_entry(
        device,
        quality,
        layout,
        code,
        mesh,
        FragmentOutput {
            entry: "fragment",
            blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        },
        #[cfg(test)]
        counts,
    )
}
pub(super) struct FragmentOutput<'a> {
    pub entry: &'a str,
    pub blend: Option<wgpu::BlendState>,
}

pub(super) fn pipeline_entry(
    device: &wgpu::Device,
    quality: Quality,
    layout: &wgpu::BindGroupLayout,
    code: &str,
    mesh: bool,
    fragment: FragmentOutput<'_>,
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
    let attributes =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32x4];
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
            entry_point: Some(fragment.entry),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: quality.format,
                blend: fragment.blend,
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

pub(super) fn operation_words(operation: CompositeOperation) -> (u32, f32, f32) {
    match operation {
        CompositeOperation::SourceOver => (0, 0., 0.),
        CompositeOperation::AlphaMask => (1, 0., 0.),
        CompositeOperation::LuminanceMask => (2, 0., 0.),
        CompositeOperation::Crossfade(progress) | CompositeOperation::Transition(progress) => {
            (3, progress, 0.)
        }
        CompositeOperation::MaskedTransition(progress) => (4, progress, 0.),
    }
}

pub(super) fn uniform(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}
pub(super) fn validated<T>(gpu: &Gpu, create: impl FnOnce() -> T) -> Result<T, GpuError> {
    gpu.check()?;
    let scopes = [
        wgpu::ErrorFilter::Validation,
        wgpu::ErrorFilter::Internal,
        wgpu::ErrorFilter::OutOfMemory,
    ]
    .map(|filter| gpu.device.push_error_scope(filter));
    let value = create();
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
    Ok(value)
}
