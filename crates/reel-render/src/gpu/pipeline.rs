use super::{Gpu, GpuError, Quality, mesh::Vertex, targets::Target};
#[cfg(not(target_arch = "wasm32"))]
use std::future::Future;
use std::sync::{Arc, Mutex};
#[cfg(not(target_arch = "wasm32"))]
use std::task::{Context, Poll, Waker};
use wgpu::util::DeviceExt;

#[cfg(test)]
#[path = "pipeline/overwrite_tests.rs"]
mod overwrite_tests;

/// Only the three existing format/sample pairs can occupy these instance slots.
/// No frame data or application-accounted allocations live here.
#[derive(Default)]
pub(super) struct PipelineSlots {
    slots: [Mutex<Option<Arc<Pipelines>>>; 3],
    coverage: [Mutex<Option<Arc<wgpu::RenderPipeline>>>; 3],
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
    #[cfg(test)]
    pub(super) fn coverage_count(&self) -> usize {
        self.coverage
            .iter()
            .filter(|slot| slot.lock().unwrap().is_some())
            .count()
    }
    #[cfg(test)]
    pub(super) fn bounded_count(&self) -> usize {
        self.slots
            .iter()
            .map(|slot| {
                slot.lock().unwrap().as_ref().map_or(0, |pipelines| {
                    pipelines.bounded.as_ref().map_or(0, |p| p.len())
                        + pipelines.bounded_opaque.as_ref().map_or(0, |p| p.len())
                })
            })
            .sum()
    }
    pub(super) fn get_coverage(
        &self,
        gpu: &Gpu,
        color: Quality,
        pipelines: &Pipelines,
    ) -> Result<Arc<wgpu::RenderPipeline>, GpuError> {
        let quality = gpu
            .coverage_quality(color)
            .ok_or_else(|| GpuError::Device("unsupported coverage quality".into()))?;
        let index = match (color.format, color.samples) {
            (wgpu::TextureFormat::Rgba16Float, 4) => 0,
            (wgpu::TextureFormat::Rgba16Float, 1) => 1,
            (wgpu::TextureFormat::Rgba8Unorm, 1) => 2,
            _ => return Err(GpuError::InvalidSize),
        };
        let mut slot = self.coverage[index]
            .lock()
            .expect("coverage pipeline slot poisoned");
        if let Some(pipeline) = &*slot {
            return Ok(pipeline.clone());
        }
        let _construction = self
            .construction
            .lock()
            .expect("pipeline construction poisoned");
        gpu.check()?;
        let result = validated(gpu, || {
            Arc::new(pipeline_module(
                &gpu.device,
                quality,
                &pipelines.paint_layout,
                &pipelines.paint_shader,
                (VertexInput::Mesh, "vertex"),
                FragmentOutput {
                    entry: "fragment",
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                },
                #[cfg(test)]
                Some(&self.creations),
            ))
        })?;
        *slot = Some(result.clone());
        Ok(result)
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
        let pipelines = Arc::new(Pipelines::new(
            gpu,
            quality,
            #[cfg(test)]
            &self.creations,
        )?);
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
    pub(super) invalid_worker_pipeline: std::sync::atomic::AtomicBool,
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
    /// Integer parent pixels preceding the copied backdrop's first texel.
    pub backdrop_origin: [i32; 2],
}
pub(super) const COMPOSITE_WORDS: usize = 20;
pub(super) const COMPOSITE_BYTES: u64 = (COMPOSITE_WORDS * size_of::<u32>()) as u64;
impl CompositeParams {
    pub(super) fn uniform_words(
        &self,
        target: &Target,
        source: &wgpu::TextureView,
        mask: &wgpu::TextureView,
        backdrop: &wgpu::TextureView,
    ) -> [u32; COMPOSITE_WORDS] {
        let size = source.texture().size();
        let uv = crate::common::source_view::uv_mapping(
            target.size,
            [size.width, size.height],
            self.source_origin,
        )
        .map(f32::to_bits);
        let mapping = |view: &wgpu::TextureView| {
            let size = view.texture().size();
            crate::common::source_view::uv_mapping(target.size, [size.width, size.height], [0; 2])
                .map(f32::to_bits)
        };
        let mask_uv = mapping(mask);
        let size = backdrop.texture().size();
        let backdrop_uv = crate::common::source_view::uv_mapping(
            target.size,
            [size.width, size.height],
            self.backdrop_origin,
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
            u32::from(matches!(
                mask.texture().format(),
                wgpu::TextureFormat::R8Unorm | wgpu::TextureFormat::R16Float
            )),
            0,
            uv[0],
            uv[1],
            uv[2],
            uv[3],
            mask_uv[0],
            mask_uv[1],
            mask_uv[2],
            mask_uv[3],
            backdrop_uv[0],
            backdrop_uv[1],
            backdrop_uv[2],
            backdrop_uv[3],
        ]
    }
}
pub(super) struct CompositeInputs<'a> {
    pub source: &'a wgpu::TextureView,
    pub mask: &'a wgpu::TextureView,
    pub backdrop: &'a wgpu::TextureView,
}
pub struct Pipelines {
    instances: Mutex<[Option<Arc<wgpu::RenderPipeline>>; 22]>,
    paint_shader: wgpu::ShaderModule,
    bounded_shader: Option<wgpu::ShaderModule>,
    pub paint: wgpu::RenderPipeline,
    pub coverage: wgpu::RenderPipeline,
    #[cfg(test)]
    pub coverage_draws: std::sync::atomic::AtomicUsize,
    pub direct: [wgpu::RenderPipeline; 8],
    pub bounded: Option<[wgpu::RenderPipeline; 7]>,
    pub bounded_opaque: Option<[wgpu::RenderPipeline; 3]>,
    #[cfg(test)]
    pub direct_draws: [std::sync::atomic::AtomicUsize; 8],
    pub opaque: [wgpu::RenderPipeline; 3],
    #[cfg(test)]
    pub opaque_draws: [std::sync::atomic::AtomicUsize; 3],
    pub yuv: wgpu::RenderPipeline,
    pub yuv_layout: wgpu::BindGroupLayout,
    pub composite: wgpu::RenderPipeline,
    pub source_over: wgpu::RenderPipeline,
    pub effect_resample: [wgpu::RenderPipeline; 2],
    pub alpha_mask: wgpu::RenderPipeline,
    pub composite_blend: wgpu::RenderPipeline,
    pub extended_blend: wgpu::RenderPipeline,
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
    fn new(gpu: &Gpu, quality: Quality, #[cfg(test)] counts: &Creations) -> Result<Self, GpuError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::PipelineCreate);
        let device = &gpu.device;
        let (
            paint_layout,
            composite_layout,
            yuv_layout,
            paint_shader,
            bounded_shader,
            composite_shader,
            bounded_composite_shader,
            samplers,
            nearest_samplers,
            plain_appearance,
            block_appearance,
        ) = validation_scope(gpu, || {
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
            let composite_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
            let paint_shader = shader_module(
                device,
                concat!(
                    include_str!("paint.wgsl"),
                    "\n",
                    include_str!("instance_vertex.wgsl")
                ),
                #[cfg(test)]
                Some(counts),
            );
            let bounded_supported = quality.format == wgpu::TextureFormat::Rgba8Unorm
                && device.features().contains(wgpu::Features::SHADER_F16);
            let bounded_shader = bounded_supported.then(|| {
                shader_module(
                    device,
                    concat!(
                        include_str!("paint_bounded.wgsl"),
                        "\n",
                        include_str!("instance_vertex.wgsl")
                    ),
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
            let composite_code = include_str!("composite.wgsl");
            let composite_shader = shader_module(
                device,
                composite_code,
                #[cfg(test)]
                Some(counts),
            );
            let bounded_composite = crate::common::precision::composite_wgsl(
                composite_code,
                quality.format == wgpu::TextureFormat::Rgba8Unorm,
                device.features().contains(wgpu::Features::SHADER_F16),
            );
            let bounded_composite_shader = match bounded_composite {
                std::borrow::Cow::Borrowed(_) => None,
                std::borrow::Cow::Owned(code) => Some(shader_module(
                    device,
                    &code,
                    #[cfg(test)]
                    Some(counts),
                )),
            };
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
                        mipmap_filter: if filter == wgpu::FilterMode::Nearest {
                            wgpu::MipmapFilterMode::Nearest
                        } else {
                            wgpu::MipmapFilterMode::Linear
                        },
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
                contents: &[0u8; 160],
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let block_appearance = std::array::from_fn(|index| {
                let mut values = [[0f32; 4]; 10];
                values[8][2] = (index + 1) as f32;
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("fixed block texture appearance"),
                    contents: bytemuck::cast_slice(&values),
                    usage: wgpu::BufferUsages::UNIFORM,
                })
            });
            (
                paint_layout,
                composite_layout,
                yuv_layout,
                paint_shader,
                bounded_shader,
                composite_shader,
                bounded_composite_shader,
                samplers,
                nearest_samplers,
                plain_appearance,
                block_appearance,
            )
        })?;

        let over = Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING);
        let paint_pipeline = |shader: &wgpu::ShaderModule, entry, blend| {
            pipeline_module(
                device,
                quality,
                &paint_layout,
                shader,
                (VertexInput::Mesh, "vertex"),
                FragmentOutput { entry, blend },
                #[cfg(test)]
                Some(counts),
            )
        };
        let compile_paint = || {
            validation_scope(gpu, || {
                ["fragment", "fragment_coverage"]
                    .map(|entry| paint_pipeline(&paint_shader, entry, over))
            })
        };
        let compile_yuv = || {
            validation_scope(gpu, || {
                pipeline_module(
                    device,
                    quality,
                    &yuv_layout,
                    &paint_shader,
                    (VertexInput::Mesh, "vertex"),
                    FragmentOutput {
                        entry: "fragment_yuv",
                        blend: over,
                    },
                    #[cfg(test)]
                    Some(counts),
                )
            })
        };
        let compile_composite = || {
            validation_scope(gpu, || {
                let composite_pipeline = |shader: &wgpu::ShaderModule, entry, blend| {
                    pipeline_module(
                        device,
                        quality,
                        &composite_layout,
                        shader,
                        (VertexInput::None, "vertex"),
                        FragmentOutput { entry, blend },
                        #[cfg(test)]
                        Some(counts),
                    )
                };
                let resample_shader = bounded_composite_shader
                    .as_ref()
                    .unwrap_or(&composite_shader);
                let composite = composite_pipeline(&composite_shader, "fragment", over);
                let composite_blend = composite_pipeline(&composite_shader, "fragment", None);
                let extended_blend = composite_pipeline(&composite_shader, "extended_blend", None);
                let effect_resample = [None, over]
                    .map(|blend| composite_pipeline(resample_shader, "effect_resample", blend));
                let source_over = composite_pipeline(resample_shader, "source_over", over);
                let alpha_mask = composite_pipeline(resample_shader, "alpha_mask", over);
                (
                    composite,
                    composite_blend,
                    extended_blend,
                    effect_resample,
                    source_over,
                    alpha_mask,
                )
            })
        };
        let compile_specialized = || {
            validation_scope(gpu, || {
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
                .map(|entry| paint_pipeline(&paint_shader, entry, over));
                let bounded = bounded_shader.as_ref().map(|shader| {
                    [
                        "fragment_direct",
                        "fragment_direct_bc1",
                        "fragment_direct_bc3",
                        "fragment_solid",
                        "fragment_regional",
                        "fragment_regional_bc1",
                        "fragment_regional_bc3",
                    ]
                    .map(|entry| paint_pipeline(shader, entry, over))
                });
                let bounded_opaque = bounded_shader.as_ref().map(|shader| {
                    [
                        "fragment_direct_bc1",
                        "fragment_solid",
                        "fragment_regional_bc1",
                    ]
                    .map(|entry| paint_pipeline(shader, entry, None))
                });
                let opaque = [
                    "fragment_direct_bc1",
                    "fragment_regional_bc1",
                    "fragment_solid",
                ]
                .map(|entry| paint_pipeline(&paint_shader, entry, None));
                (direct, bounded, bounded_opaque, opaque)
            })
        };
        // Each worker owns its WGPU error scopes.
        #[cfg(not(target_arch = "wasm32"))]
        let (paint, specialized, yuv, composite) = std::thread::scope(|scope| {
            let paint = scope.spawn(compile_paint);
            let yuv = scope.spawn(compile_yuv);
            let composite = scope.spawn(compile_composite);
            let specialized = compile_specialized();
            (
                paint.join().unwrap(),
                specialized,
                yuv.join().unwrap(),
                composite.join().unwrap(),
            )
        });
        #[cfg(target_arch = "wasm32")]
        let (paint, specialized, yuv, composite) = (
            compile_paint(),
            compile_specialized(),
            compile_yuv(),
            compile_composite(),
        );
        let [paint, coverage] = paint?;
        let (direct, bounded, bounded_opaque, opaque) = specialized?;
        let (composite, composite_blend, extended_blend, effect_resample, source_over, alpha_mask) =
            composite?;
        Ok(Self {
            instances: Default::default(),
            paint_shader,
            bounded_shader,
            paint,
            coverage,
            #[cfg(test)]
            coverage_draws: Default::default(),
            direct,
            bounded,
            bounded_opaque,
            #[cfg(test)]
            direct_draws: Default::default(),
            opaque,
            #[cfg(test)]
            opaque_draws: Default::default(),
            yuv: yuv?,
            yuv_layout,
            composite,
            source_over,
            effect_resample,
            alpha_mask,
            composite_blend,
            extended_blend,
            paint_layout,
            composite_layout,
            samplers,
            nearest_samplers,
            plain_appearance,
            block_appearance,
            masks: super::mask::Cache::default(),
            effects: super::effect::Cache::default(),
        })
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
        let words = params.uniform_words(target, source, mask, backdrop);
        self.composite_group_words(
            device,
            CompositeInputs {
                source,
                mask,
                backdrop,
            },
            &words,
        )
    }
    pub(super) fn composite_group_words(
        &self,
        device: &wgpu::Device,
        inputs: CompositeInputs<'_>,
        words: &[u32; COMPOSITE_WORDS],
    ) -> wgpu::BindGroup {
        let CompositeInputs {
            source,
            mask,
            backdrop,
        } = inputs;
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("composite params"),
            contents: bytemuck::cast_slice(words),
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
        } else if params.blend > 4 {
            &self.extended_blend
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
        let full = crate::common::target_writes::FullOverwrite::composite(
            target.size,
            target.storage_size(),
            params.scissor,
            params.blend != 0 || params.operation.replaces(),
        );
        let mut attachment = target.attachment(params.clear);
        if let Some(proof) = full {
            attachment.ops.load = super::targets::overwrite_load(proof);
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("whole group composite"),
            color_attachments: &[Some(attachment)],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            multiview_mask: None,
            occlusion_query_set: None,
        });
        pass.set_viewport(0., 0., target.size[0] as f32, target.size[1] as f32, 0., 1.);
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
        pass.set_viewport(0., 0., target.size[0] as f32, target.size[1] as f32, 0., 1.);
        for (group, params) in &groups {
            pass.set_pipeline(self.composite_pipeline(params));
            params.scissor.apply(&mut pass);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
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
    pipeline_vertex(
        device,
        quality,
        layout,
        code,
        (
            if mesh {
                VertexInput::Mesh
            } else {
                VertexInput::None
            },
            "vertex",
        ),
        fragment,
        #[cfg(test)]
        counts,
    )
}

pub(super) enum VertexInput {
    None,
    Mesh,
    Instances,
}

pub(super) fn pipeline_vertex(
    device: &wgpu::Device,
    quality: Quality,
    layout: &wgpu::BindGroupLayout,
    code: &str,
    vertex: (VertexInput, &str),
    fragment: FragmentOutput<'_>,
    #[cfg(test)] counts: Option<&Creations>,
) -> wgpu::RenderPipeline {
    let shader = shader_module(
        device,
        code,
        #[cfg(test)]
        counts,
    );
    pipeline_module(
        device,
        quality,
        layout,
        &shader,
        vertex,
        fragment,
        #[cfg(test)]
        counts,
    )
}

pub(super) fn shader_module(
    device: &wgpu::Device,
    code: &str,
    #[cfg(test)] counts: Option<&Creations>,
) -> wgpu::ShaderModule {
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
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("reel linear shader"),
        source: wgpu::ShaderSource::Wgsl(code.into()),
    })
}

pub(super) fn pipeline_module(
    device: &wgpu::Device,
    quality: Quality,
    layout: &wgpu::BindGroupLayout,
    shader: &wgpu::ShaderModule,
    vertex: (VertexInput, &str),
    fragment: FragmentOutput<'_>,
    #[cfg(test)] counts: Option<&Creations>,
) -> wgpu::RenderPipeline {
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
    pipeline_with_layout(
        device,
        quality,
        &pipeline_layout,
        shader,
        vertex,
        fragment,
        #[cfg(test)]
        counts,
    )
}

pub(super) fn pipeline_with_layout(
    device: &wgpu::Device,
    quality: Quality,
    pipeline_layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    vertex: (VertexInput, &str),
    fragment: FragmentOutput<'_>,
    #[cfg(test)] counts: Option<&Creations>,
) -> wgpu::RenderPipeline {
    let attributes =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32x4];
    let buffers = [Some(wgpu::VertexBufferLayout {
        array_stride: size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attributes,
    })];
    let base_attributes = instance_attributes(crate::common::quad_instances::VERTEX_ATTRIBUTES);
    let per_instance = instance_attributes(crate::common::quad_instances::INSTANCE_ATTRIBUTES);
    let instance_buffers = [
        Some(wgpu::VertexBufferLayout {
            array_stride: size_of::<crate::common::quad_instances::QuadVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &base_attributes,
        }),
        Some(wgpu::VertexBufferLayout {
            array_stride: size_of::<crate::common::quad_instances::Instance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &per_instance,
        }),
    ];
    #[cfg(test)]
    if let Some(counts) = counts {
        counts
            .render_pipelines
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    let entry = fragment.entry;
    #[cfg(test)]
    let entry = if entry == "fragment_yuv"
        && counts.is_some_and(|c| {
            c.invalid_worker_pipeline
                .load(std::sync::atomic::Ordering::Relaxed)
        }) {
        "missing_worker_fragment"
    } else {
        entry
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("reel source over"),
        layout: Some(pipeline_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vertex.1),
            compilation_options: Default::default(),
            buffers: match vertex.0 {
                VertexInput::None => &[],
                VertexInput::Mesh => &buffers,
                VertexInput::Instances => &instance_buffers,
            },
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(entry),
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
fn instance_attributes<const N: usize>(
    attrs: [crate::common::quad_instances::Attribute; N],
) -> [wgpu::VertexAttribute; N] {
    attrs.map(|attr| wgpu::VertexAttribute {
        shader_location: attr.location,
        offset: attr.offset,
        format: match attr.components {
            1 => wgpu::VertexFormat::Float32,
            2 => wgpu::VertexFormat::Float32x2,
            4 => wgpu::VertexFormat::Float32x4,
            _ => unreachable!(),
        },
    })
}
#[path = "pipeline_instances.rs"]
mod instances;

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
    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::PipelineCreate);
    validation_scope(gpu, create)
}
pub(super) fn validation_scope<T>(gpu: &Gpu, create: impl FnOnce() -> T) -> Result<T, GpuError> {
    gpu.check()?;
    let scopes = [
        wgpu::ErrorFilter::Validation,
        wgpu::ErrorFilter::Internal,
        wgpu::ErrorFilter::OutOfMemory,
    ]
    .map(|filter| gpu.device.push_error_scope(filter));
    let value = create();
    #[cfg(target_arch = "wasm32")]
    for scope in scopes.into_iter().rev() {
        // Pop immediately and let the browser resolve the promise. The next
        // renderer turn sees any error in the same sticky device state.
        gpu.observe_error_scope(scope.pop());
    }
    #[cfg(not(target_arch = "wasm32"))]
    complete_error_scopes(gpu, scopes)?;
    Ok(value)
}
#[cfg(not(target_arch = "wasm32"))]
fn complete_error_scopes(gpu: &Gpu, scopes: [wgpu::ErrorScopeGuard; 3]) -> Result<(), GpuError> {
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
    Ok(())
}
