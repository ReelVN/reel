//! Prepared WGSL products with the same source/parameter ABI as GLES.
use super::{Gpu, GpuError, Quality, pipeline, replay::Context, targets::Target};
use crate::common::{effect_cache, effect_plan};
use reel_core::budget::{BudgetPool, BudgetTicket};
use reel_ui::{
    frame::{FrameDrawing, FrameEffect, RecordLocation},
    kurbo::Affine,
    resources::EffectShaderFormat,
};
use std::sync::{Arc, Mutex};
use wgpu::util::DeviceExt;

#[derive(Default)]
pub(super) struct Cache(Mutex<effect_cache::Cache<Arc<Compiled>>>);
pub(super) struct Compiled {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    _storage: BudgetTicket,
}
impl Cache {
    pub(super) fn prune(&self) {
        self.0.lock().unwrap().prune();
    }
    fn get(
        &self,
        gpu: &Gpu,
        quality: Quality,
        shader: &effect_plan::Shader<'_>,
        budget: &BudgetPool,
        at: RecordLocation,
    ) -> Result<Arc<Compiled>, GpuError> {
        let effect_plan::Shader {
            lease,
            descriptor,
            bytes,
        } = *shader;
        if descriptor.format != EffectShaderFormat::Wgsl {
            return Err(GpuError::Unsupported {
                at,
                record: "WGPU requires a prepared WGSL effect shader",
            });
        }
        if descriptor.parameters.len().saturating_mul(16).max(16)
            > gpu.device.limits().max_uniform_buffer_binding_size as usize
        {
            return Err(GpuError::Unsupported {
                at,
                record: "effect uniforms exceed the device capacity",
            });
        }
        let mut cache = self.0.lock().unwrap();
        if let Some(compiled) = cache.get(
            descriptor.shader_key,
            lease,
            budget,
            gpu.limits.resource_bytes,
        )? {
            return Ok(compiled);
        }
        let source = std::str::from_utf8(bytes).map_err(|_| GpuError::InvalidRecord {
            at,
            record: "effect WGSL encoding",
        })?;
        let storage = effect_cache::reserve(
            budget,
            size_of::<Compiled>() + 2 * size_of::<usize>(),
            gpu.limits.resource_bytes,
        )?;
        let compiled = pipeline::validated(gpu, || {
            let layout = gpu
                .device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("prepared effect"),
                    entries: &[
                        pipeline::texture(0),
                        pipeline::sampler(1),
                        pipeline::uniform(2),
                        pipeline::uniform(3),
                    ],
                });
            let pipeline = pipeline::pipeline_entry(
                &gpu.device,
                quality,
                &layout,
                source,
                false,
                pipeline::FragmentOutput {
                    entry: "fragment",
                    blend: None,
                },
                #[cfg(test)]
                None,
            );
            Arc::new(Compiled {
                layout,
                pipeline,
                _storage: storage,
            })
        })
        .map_err(|error| GpuError::Shader {
            at,
            asset: lease.version().key.0.clone(),
            source: Box::new(error),
        })?;
        cache.insert(
            descriptor.shader_key,
            lease,
            compiled.clone(),
            budget,
            gpu.limits.resource_bytes,
        )?;
        Ok(compiled)
    }
}
impl Context<'_> {
    pub(super) fn effect(
        &mut self,
        drawing: &FrameDrawing,
        effect: &FrameEffect,
        transform: Affine,
        source: &Target,
        at: RecordLocation,
    ) -> Result<Target, GpuError> {
        self.flush_draws()?;
        let shader = effect_plan::shader(drawing, effect, at)?;
        let compiled = self.pipelines.effects.get(
            self.gpu,
            self.quality,
            &shader,
            &self.budgets.resources,
            at,
        )?;
        let output = self.target()?;
        let Some(context) = effect_plan::context(effect, transform, self.size, source.size) else {
            return Ok(output);
        };
        if self.scissor.empty() {
            return Ok(output);
        }
        // Charge CPU packing plus the two independent uniform buffer allocations.
        let parameter_bytes = effect.values.len().max(1) * 16;
        self.transient((2 * parameter_bytes + size_of_val(&context)) as u64)?;
        let parameters = if effect.values.is_empty() {
            vec![[0.; 4]]
        } else {
            effect
                .values
                .iter()
                .copied()
                .map(effect_plan::slot)
                .collect()
        };
        let uniform = |label, data: &[u8]| {
            self.gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents: data,
                    usage: wgpu::BufferUsages::UNIFORM,
                })
        };
        let context = uniform("effect coordinates", bytemuck::cast_slice(&context));
        let parameters = uniform("effect parameters", bytemuck::cast_slice(&parameters));
        let bindings = self
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("prepared effect"),
                layout: &compiled.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&source.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.pipelines.samplers[0]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: context.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: parameters.as_entire_binding(),
                    },
                ],
            });
        let mut held = std::mem::take(&mut self.effects);
        let retained = crate::common::scopes::push_stack(&mut held, compiled.clone(), |bytes| {
            self.transient(bytes)
        });
        self.effects = held;
        retained?;
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::EffectComposite);
        let mut pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("prepared effect"),
            color_attachments: &[Some(output.attachment(false))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            multiview_mask: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&compiled.pipeline);
        pass.set_bind_group(0, &bindings, &[]);
        pass.set_scissor_rect(
            self.scissor.x,
            self.scissor.y,
            self.scissor.width,
            self.scissor.height,
        );
        pass.draw(0..3, 0..1);
        drop(pass);
        Ok(output)
    }
}
