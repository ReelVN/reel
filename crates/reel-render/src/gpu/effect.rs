//! Prepared WGSL products with the same source/parameter ABI as GXM.
use super::{Gpu, GpuError, Quality, pipeline, replay::Context, targets::Target};
use crate::common::{effect_cache, effect_plan};
use reel_core::budget::{BudgetPool, BudgetTicket};
use reel_ui::{
    frame::{FrameDrawing, FrameEffect, RecordLocation},
    kurbo::Affine,
    resources::EffectShaderFormat,
};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(super) struct Cache(Mutex<[effect_cache::Cache<Arc<Compiled>>; 8]>);
pub(super) struct Compiled {
    pub(super) layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    _storage: BudgetTicket,
}
impl Cache {
    pub(super) fn prune(&self) {
        for cache in &mut *self.0.lock().unwrap() {
            cache.prune();
        }
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "compiled effect variants and their budget owner"
    )]
    fn get(
        &self,
        gpu: &Gpu,
        quality: Quality,
        padded: bool,
        terminal: bool,
        masked: bool,
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
        if descriptor
            .textures
            .saturating_add(u32::from(descriptor.source_sampler))
            .saturating_add(u32::from(masked))
            > gpu.device.limits().max_sampled_textures_per_shader_stage
            || descriptor.textures as u64 * 32 > gpu.device.limits().max_uniform_buffer_binding_size
        {
            return Err(GpuError::Unsupported {
                at,
                record: "effect textures exceed the device capacity",
            });
        }
        let mut caches = self.0.lock().unwrap();
        let cache =
            &mut caches[usize::from(padded) + 2 * usize::from(terminal) + 4 * usize::from(masked)];
        if let Some(compiled) = cache.get(
            descriptor.shader_key,
            lease,
            budget,
            gpu.limits.resource_bytes,
        )? {
            return Ok(compiled);
        }
        let bytes = descriptor
            .code(bytes, masked)
            .ok_or(GpuError::InvalidRecord {
                at,
                record: "missing effect variant",
            })?;
        let source = std::str::from_utf8(bytes).map_err(|_| GpuError::InvalidRecord {
            at,
            record: "effect WGSL encoding",
        })?;
        let storage = effect_cache::reserve(
            budget,
            size_of::<Compiled>() + 2 * size_of::<usize>(),
            gpu.limits.resource_bytes,
        )?;
        let entry_capacity = 3
            + usize::from(descriptor.source_sampler)
            + usize::from(descriptor.textures != 0)
            + descriptor.textures as usize
            + usize::from(masked);
        let _layout_storage = effect_cache::reserve(
            budget,
            entry_capacity * size_of::<wgpu::BindGroupLayoutEntry>(),
            gpu.limits.resource_bytes,
        )?;
        let compiled = pipeline::validated(gpu, || {
            let mut entries = Vec::with_capacity(entry_capacity);
            entries.extend([
                pipeline::sampler(1),
                wgpu::BindGroupLayoutEntry {
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ..pipeline::uniform(2)
                },
                pipeline::uniform(3),
            ]);
            if descriptor.source_sampler {
                entries.push(pipeline::texture(0));
            }
            if descriptor.textures != 0 {
                entries.push(pipeline::uniform(4));
                entries.extend((0..descriptor.textures).map(|i| pipeline::texture(5 + i)));
            }
            if masked {
                entries.push(pipeline::texture(5 + descriptor.textures));
            }
            let layout = gpu
                .device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("prepared effect"),
                    entries: &entries,
                });
            let pipeline = pipeline::pipeline_vertex(
                &gpu.device,
                quality,
                &layout,
                source,
                (
                    if padded {
                        pipeline::VertexInput::Mesh
                    } else {
                        pipeline::VertexInput::None
                    },
                    if padded { "vertex_padded" } else { "vertex" },
                ),
                pipeline::FragmentOutput {
                    entry: "fragment",
                    blend: terminal.then_some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
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
        source_commands: Option<(std::ops::Range<usize>, Affine)>,
        at: RecordLocation,
    ) -> Result<Target, GpuError> {
        use crate::common::effect_result::{Grid, Probe};
        self.flush_draws()?;
        let proof_transform = source_commands
            .as_ref()
            .map_or(Affine::IDENTITY, |(_, value)| *value);
        let (source_transform, effect_transform) = Grid::transforms(proof_transform, transform);
        let grid = Grid {
            source_transform,
            effect_transform,
            source_size: source.size,
            source_storage: source.storage_size(),
            output_size: self.size,
            output_storage: self.offscreen.storage_size(self.size),
            scissor: [
                self.scissor.x,
                self.scissor.y,
                self.scissor.width,
                self.scissor.height,
            ],
            quality: [
                match self.quality.format {
                    wgpu::TextureFormat::Rgba8Unorm => 0,
                    wgpu::TextureFormat::Rgba16Float => 1,
                    _ => {
                        return self.effect_draw(
                            drawing,
                            effect,
                            transform,
                            effect_plan::Source::sampled(
                                source,
                                source.size,
                                source.storage_size(),
                            ),
                            None,
                            None,
                            at,
                        );
                    }
                },
                self.quality.samples,
            ],
        };
        let probe = if !self.capture
            && !self.scissor.empty()
            && effect_plan::context(effect, transform, self.size, source.size).is_some()
        {
            if let Some(token) = source.effect_token() {
                Some(Probe::from_output(token, effect, grid))
            } else if let Some((commands, _)) = source_commands {
                Probe::new(drawing, commands, effect, grid)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(probe) = probe {
            if let Some((_token, hit)) = self.effect_results.lookup(&probe) {
                let bytes = super::targets::target_bytes(hit.target.storage_size(), self.quality)?;
                let local =
                    self.budget_scope
                        .targets
                        .reserve(bytes)
                        .map_err(|_| GpuError::Budget {
                            domain: "offscreen targets",
                            requested: bytes,
                            available: self
                                .gpu
                                .limits
                                .target_bytes
                                .saturating_sub(self.budget_scope.targets.usage().bytes),
                        })?;
                self.tickets.push(crate::common::renderer::Reservation {
                    _shared: None,
                    _local: local,
                });
                let mut owners = std::mem::take(&mut self.retained_inputs);
                let held = crate::common::scopes::push_stack(
                    &mut owners,
                    hit.allocation.clone(),
                    |bytes| self.transient(bytes),
                );
                self.retained_inputs = owners;
                held?;
                let mut pins = std::mem::take(&mut self.effect_pins);
                let held =
                    crate::common::scopes::push_stack(&mut pins, hit.target.clone(), |bytes| {
                        self.transient(bytes)
                    });
                self.effect_pins = pins;
                held?;
                return Ok(hit.target);
            }
            let candidate = self.effect_results.prepare(probe, &self.budgets.transient);
            let output = self.effect_draw(
                drawing,
                effect,
                transform,
                effect_plan::Source::sampled(source, source.size, source.storage_size()),
                None,
                None,
                at,
            )?;
            if let Some(candidate) = candidate
                && let Some(pinned) = output.pin_effect(candidate.token, &self.budgets.transient)
            {
                let mut pins = std::mem::take(&mut self.effect_pins);
                let held = crate::common::scopes::push_stack(&mut pins, pinned.clone(), |bytes| {
                    self.transient(bytes)
                });
                self.effect_pins = pins;
                if held.is_ok() {
                    let allocation = self.target_allocations[output.slot].clone();
                    let stored = super::retained::Output {
                        allocation,
                        target: pinned.clone(),
                    };
                    let mut pending = std::mem::take(&mut self.effect_pending);
                    let saved = crate::common::scopes::push_stack(
                        &mut pending,
                        candidate.finish(stored),
                        |bytes| self.transient(bytes),
                    );
                    self.effect_pending = pending;
                    if saved.is_ok() {
                        return Ok(pinned);
                    }
                    self.effect_pins.pop();
                }
            }
            return Ok(output);
        }
        self.effect_draw(
            drawing,
            effect,
            transform,
            effect_plan::Source::sampled(source, source.size, source.storage_size()),
            None,
            None,
            at,
        )
    }
    /// Multiply the complete local source by scalar coverage in one pass.
    pub(super) fn effect_mask(
        &mut self,
        drawing: &FrameDrawing,
        effect: &FrameEffect,
        transform: Affine,
        source: &Target,
        source_frame_to_pixels: Affine,
        at: RecordLocation,
    ) -> Result<Target, GpuError> {
        self.effect_draw(
            drawing,
            effect,
            transform,
            effect_plan::Source::sampled(source, source.size, source.storage_size()),
            Some(source_frame_to_pixels),
            None,
            at,
        )
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "terminal effect retains its source and destination domains"
    )]
    pub(super) fn effect_into(
        &mut self,
        drawing: &FrameDrawing,
        effect: &FrameEffect,
        transform: Affine,
        source: &Target,
        target: &Target,
        opacity: f32,
        coverage: Option<(&Target, bool)>,
        at: RecordLocation,
    ) -> Result<(), GpuError> {
        self.effect_draw(
            drawing,
            effect,
            transform,
            effect_plan::Source::sampled(source, source.size, source.storage_size()),
            None,
            Some(effect_plan::Terminal {
                target,
                opacity,
                coverage,
            }),
            at,
        )
        .map(|_| ())
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "one prepared effect and optional terminal composite"
    )]
    pub(super) fn effect_draw(
        &mut self,
        drawing: &FrameDrawing,
        effect: &FrameEffect,
        transform: Affine,
        source: effect_plan::Source<'_, Target>,
        source_view: Option<Affine>,
        terminal: Option<effect_plan::Terminal<'_, Target>>,
        at: RecordLocation,
    ) -> Result<Target, GpuError> {
        self.flush_draws()?;
        let coverage = terminal.as_ref().and_then(|output| output.coverage);
        let nested = coverage.is_some_and(|(_, nested)| nested);
        let coverage = coverage.map(|(mask, _)| mask);
        let shader = effect_plan::shader(drawing, effect, at)?;
        let padded = shader.descriptor.output == reel_ui::stage::effect::EffectType::Float
            && source.size != source.storage_size;
        let compiled = self.pipelines.effects.get(
            self.gpu,
            self.quality,
            padded,
            terminal.is_some(),
            coverage.is_some(),
            &shader,
            &self.budgets.resources,
            at,
        )?;
        let output = if let Some(output) = &terminal {
            output.target.clone()
        } else {
            self.target()?
        };
        let Some(mut context) = effect_plan::context(effect, transform, self.size, source.size)
        else {
            return Ok(output);
        };
        if let Some(view) = source_view {
            context[0][3] = if effect.coverage_invert { 3.0 } else { 0.0 };
            effect_plan::map_source(&mut context, effect, transform, view, source.size);
        }
        if let Some(effect_plan::Terminal { opacity, .. }) = terminal {
            context[1][3] = if self.quality.format == wgpu::TextureFormat::Rgba16Float {
                2.0
            } else {
                1.0
            };
            context[2][3] = opacity;
            context[3][3] = f32::from(nested);
        }
        effect_plan::map_storage(&mut context, source.size, source.storage_size);
        if let Some(mask) = coverage {
            context[9] =
                crate::common::source_view::uv_mapping(self.size, mask.storage_size(), [0; 2]);
        }
        if self.scissor.empty() {
            return Ok(output);
        }
        if let Some(mask) = coverage {
            self.ensure_sampled(&mask.view);
        }
        if shader.descriptor.source_sampler {
            self.ensure_sampled(&source.pixels.expect("sampled effect source").view);
        }
        // The slot owns its GPU buffer and exact texture leases until completion.
        let entry_capacity = 3
            + usize::from(shader.descriptor.source_sampler)
            + usize::from(!effect.images.is_empty())
            + effect.images.len()
            + usize::from(coverage.is_some());
        self.transient(
            (entry_capacity * size_of::<wgpu::BindGroupEntry<'_>>()
                + 2 * effect.images.len() * size_of::<Arc<super::resources::Image>>())
                as u64,
        )?;
        let mut images = Vec::with_capacity(effect.images.len());
        for handle in &effect.images {
            images.push(
                self.images
                    .get(handle)
                    .ok_or(GpuError::InvalidRecord {
                        at,
                        record: "effect texture is not resident",
                    })?
                    .clone(),
            );
        }
        let context_slots = context;
        let source_target = shader
            .descriptor
            .source_sampler
            .then(|| source.pixels.expect("sampled effect source"));
        let prepared = self.effect_uniform_cache.prepare(
            self.gpu,
            self.budgets,
            &self.budget_scope,
            compiled.clone(),
            source_target,
            coverage,
            images.clone(),
            &context_slots,
            &effect.values,
            !self.capture,
            &self.pipelines.samplers[0],
        );
        let fallback_bindings = match prepared {
            Ok((slot, local)) => {
                self.tickets.push(crate::common::renderer::Reservation {
                    _shared: None,
                    _local: local,
                });
                let mut slots = std::mem::take(&mut self.effect_uniform_slots);
                let pushed = crate::common::scopes::push_stack(&mut slots, slot, |bytes| {
                    self.transient(bytes)
                });
                self.effect_uniform_slots = slots;
                pushed?;
                None
            }
            Err(GpuError::Budget { .. } | GpuError::Pressure { .. }) => {
                let parameter_bytes = effect.values.len().max(1) * 16;
                let metadata_bytes = images.len() * 32;
                self.transient(
                    (2 * parameter_bytes + size_of_val(&context_slots) + 2 * metadata_bytes) as u64,
                )?;
                Some(super::effect_uniform::fallback(
                    self.gpu,
                    &compiled,
                    source_target,
                    coverage,
                    &images,
                    &context_slots,
                    &effect.values,
                    &self.pipelines.samplers[0],
                ))
            }
            Err(error) => return Err(error),
        };
        let mut held = std::mem::take(&mut self.effects);
        let retained = crate::common::scopes::push_stack(&mut held, compiled.clone(), |bytes| {
            self.transient(bytes)
        });
        self.effects = held;
        retained?;
        let mut index_count = 0;
        let mesh = if padded {
            let geometry = effect_plan::implicit_mesh::geometry(&context_slots)
                .ok_or(GpuError::InvalidSize)?;
            let mut mesh = crate::common::mesh::Mesh::new(self.mesh_allocation());
            mesh.indexed_triangles(geometry.vertices(), geometry.indices().iter().copied())?;
            index_count = geometry.indices().len() as u32;
            let batch = crate::common::draw_batch::Batch::new(mesh, ())?;
            let (storage, local, metadata) = self.geometry_cache.upload_batch(
                self.gpu,
                self.budgets,
                &self.budget_scope,
                &batch,
                self.geometry_policy,
                &mut self.encoder,
            )?;
            self.tickets.push(super::renderer::Reservation {
                _shared: None,
                _local: local,
            });
            if let Some(local) = metadata {
                self.tickets.push(super::renderer::Reservation {
                    _shared: None,
                    _local: local,
                });
            }
            let mut held = std::mem::take(&mut self.geometry);
            let result = crate::common::scopes::push_stack(&mut held, storage, |bytes| {
                self.transient(bytes)
            });
            self.geometry = held;
            result?;
            Some(&self.geometry.last().unwrap().storage)
        } else {
            None
        };
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::EffectComposite);
        let mut pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("prepared effect"),
            color_attachments: &[Some(
                output.attachment(self.target_writes.take(output.slot)),
            )],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            multiview_mask: None,
            occlusion_query_set: None,
        });
        pass.set_viewport(0., 0., output.size[0] as f32, output.size[1] as f32, 0., 1.);
        pass.set_pipeline(&compiled.pipeline);
        let bindings = fallback_bindings.as_ref().unwrap_or_else(|| {
            self.effect_uniform_slots
                .last()
                .expect("prepared effect slot")
                .group()
        });
        pass.set_bind_group(0, bindings, &[]);
        pass.set_scissor_rect(
            self.scissor.x,
            self.scissor.y,
            self.scissor.width,
            self.scissor.height,
        );
        if let Some(mesh) = mesh {
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..index_count, 0, 0..1);
        } else {
            pass.draw(0..3, 0..1);
        }
        drop(pass);
        Ok(output)
    }
}

#[cfg(test)]
mod tests;
