//! Prepared asset programs and shared effect execution.
use super::{GpuError, native, programs, replay::Context, resources::Target};
use crate::common::{effect_cache, effect_plan};
use reel_core::budget::BudgetPool;
use reel_ui::{
    frame::{FrameDrawing, FrameEffect, RecordLocation},
    kurbo::Affine,
};
use std::rc::Rc;

#[derive(Default)]
pub(super) struct Cache([effect_cache::Cache<Rc<Compiled>>; 5]);
impl Cache {
    pub fn prune(&mut self) {
        for cache in &mut self.0 {
            cache.prune();
        }
    }
}
pub(super) struct Compiled {
    program: native::Program,
    source: Option<native::Uniform>,
    coverage: Option<native::Uniform>,
    context: Option<native::Uniform>,
    parameters: Vec<Option<native::Uniform>>,
    textures: Vec<Option<native::Uniform>>,
    texture_meta: Option<native::Uniform>,
    _storage: super::ownership::NativeTicket,
}

#[expect(
    clippy::too_many_arguments,
    reason = "one prepared fragment and its fixed execution variant"
)]
fn compile(
    gpu: &super::ownership::Device,
    shader: &effect_plan::Shader<'_>,
    budget: &BudgetPool,
    limit: u64,
    at: RecordLocation,
    padded: bool,
    masked: bool,
    full_target: bool,
) -> Result<Rc<Compiled>, GpuError> {
    let effect_plan::Shader {
        lease,
        descriptor,
        bytes,
    } = *shader;
    let count = descriptor.parameters.len();
    let format = descriptor.format;
    if descriptor
        .textures
        .saturating_add(u32::from(descriptor.source_sampler))
        .saturating_add(u32::from(masked))
        > gpu.capabilities().max_texture_units
    {
        return Err(GpuError::Unsupported {
            at,
            record: "effect textures exceed the device capacity",
        });
    }
    let bytes = if full_target {
        descriptor.full_target_code(bytes)
    } else {
        descriptor.code(bytes, masked)
    }
    .ok_or(GpuError::InvalidRecord {
        at,
        record: "missing effect variant",
    })?;
    let code = super::super::effect_code(format, bytes, at)?;
    let storage = effect_cache::reserve(
        budget,
        size_of::<Compiled>()
            + 2 * size_of::<usize>()
            + (count + descriptor.textures as usize) * size_of::<Option<native::Uniform>>(),
        limit,
    )?;
    let storage = gpu.ticket(storage);
    let program = programs::create_program(
        gpu,
        code,
        descriptor.output == reel_ui::stage::effect::EffectType::Float || masked,
        padded,
        !full_target,
    )
    .map_err(|error| GpuError::Shader {
        at,
        asset: lease.version().key.0.clone(),
        source: Box::new(error),
    })?;
    let source = program.uniform("reel_source");
    let context = program.uniform("reel_context[0]");
    let parameters = (0..count)
        .map(|i| program.uniform(&format!("reel_parameter_{i}")))
        .collect();
    Ok(Rc::new(Compiled {
        source,
        coverage: program.uniform("reel_composite_mask"),
        context,
        parameters,
        textures: (0..descriptor.textures)
            .map(|i| program.uniform(&format!("reel_texture_{i}")))
            .collect(),
        texture_meta: program.uniform("reel_texture_meta[0]"),
        program,
        _storage: storage,
    }))
}

impl Context<'_> {
    pub(super) fn cached_effect(
        &mut self,
        probe: &crate::common::effect_result::Probe<'_>,
    ) -> Result<Option<Target>, GpuError> {
        let Some((token, hit)) = self.effect_results.lookup(probe) else {
            return Ok(None);
        };
        if self
            .effect_pins
            .iter()
            .any(|held| held.effect_token() == Some(token))
        {
            return Ok(Some(hit));
        }
        let bytes = self
            .gpu
            .target_layout(hit.view.size(), native::TargetFormat::Rgba8)?
            .texture_bytes;
        if !self.effect_admission.can_retain(bytes) {
            return Ok(None);
        }
        let local = self
            .budget_scope
            .targets
            .reserve(bytes)
            .map_err(|_| GpuError::Budget {
                domain: "offscreen targets",
                requested: bytes,
                available: self
                    .limits
                    .target_bytes
                    .saturating_sub(self.budget_scope.targets.usage().bytes),
            })?;
        self.tickets.push(crate::common::renderer::Reservation {
            _shared: None,
            _local: local,
        });
        let mut pins = std::mem::take(&mut self.effect_pins);
        let held = crate::common::scopes::push_stack(&mut pins, hit.clone(), |bytes| {
            self.transient(bytes)
        });
        self.effect_pins = pins;
        held?;
        self.effect_admission.retain(bytes);
        Ok(Some(hit))
    }

    pub(super) fn effect(
        &mut self,
        drawing: &FrameDrawing,
        effect: &FrameEffect,
        transform: Affine,
        source: &Target,
        source_commands: Option<(
            std::ops::Range<usize>,
            Affine,
            crate::common::scissor::Scissor,
        )>,
        at: RecordLocation,
    ) -> Result<Target, GpuError> {
        use crate::common::effect_result::{Grid, Probe};
        self.flush_draws()?;
        let proof_transform = source_commands
            .as_ref()
            .map_or(Affine::IDENTITY, |(_, value, _)| *value);
        let region = source_commands.as_ref().map_or(
            crate::common::scissor::Scissor::full(source.size),
            |(_, _, region)| *region,
        );
        let (source_transform, effect_transform) = Grid::transforms(proof_transform, transform);
        let grid = Grid {
            source_transform,
            effect_transform,
            source_size: source.size,
            source_storage: source.view.size(),
            source_scissor: [region.x, region.y, region.width, region.height],
            output_size: self.size,
            output_storage: self.offscreen.storage_size(self.size),
            scissor: [
                self.scissor.x,
                self.scissor.y,
                self.scissor.width,
                self.scissor.height,
            ],
            quality: [0, 1],
        };
        let probe = if self.effect_admission.enabled()
            && !self.capture
            && !self.scissor.empty()
            && effect_plan::context(effect, transform, self.size, source.size).is_some()
        {
            if let Some(token) = source.effect_token() {
                Some(Probe::from_output(token, effect, grid))
            } else if let Some((commands, _, _)) = source_commands {
                Probe::new(drawing, commands, effect, grid)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(probe) = probe {
            if let Some(hit) = self.cached_effect(&probe)? {
                #[cfg(feature = "profiling")]
                crate::profiling::record_effect(crate::profiling::EffectSample {
                    definition: effect.definition,
                    variant: 3,
                    hits: 1,
                    ..Default::default()
                });
                return Ok(hit);
            }
            let output = self.effect_draw(
                drawing,
                effect,
                transform,
                effect_plan::Source::sampled(source, source.size, source.view.size()),
                None,
                None,
                at,
            )?;
            let bytes = self
                .gpu
                .target_layout(output.view.size(), native::TargetFormat::Rgba8)?
                .texture_bytes;
            if self.effect_admission.can_retain(bytes)
                && let Some(candidate) = self.effect_results.prepare(probe, &self.budgets.transient)
            {
                // CDRAM kernel blocks are aligned to 256 KiB. Each new pin can
                // require another scratch block; resident cache hits already own theirs.
                let physical_bytes = bytes.next_multiple_of(256 * 1024);
                if !candidate.retain || !self.effect_admission.can_retain_new(bytes, physical_bytes)
                {
                    let mut pending = std::mem::take(&mut self.effect_pending);
                    let _ = crate::common::scopes::push_stack(
                        &mut pending,
                        candidate.observe(),
                        |bytes| self.transient(bytes),
                    );
                    self.effect_pending = pending;
                    return Ok(output);
                }
                let Some(pinned) = output.pin_effect(candidate.token, &self.budgets.transient)
                else {
                    return Ok(output);
                };
                let mut pins = std::mem::take(&mut self.effect_pins);
                let held = crate::common::scopes::push_stack(&mut pins, pinned.clone(), |bytes| {
                    self.transient(bytes)
                });
                self.effect_pins = pins;
                if held.is_ok() {
                    let stored = pinned.clone();
                    let mut pending = std::mem::take(&mut self.effect_pending);
                    let saved = crate::common::scopes::push_stack(
                        &mut pending,
                        candidate.finish(stored),
                        |bytes| self.transient(bytes),
                    );
                    self.effect_pending = pending;
                    if saved.is_ok() {
                        self.effect_admission.retain_new(bytes, physical_bytes);
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
            effect_plan::Source::sampled(source, source.size, source.view.size()),
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
            effect_plan::Source::sampled(source, source.size, source.view.size()),
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
            effect_plan::Source::sampled(source, source.size, source.view.size()),
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
        let effect_plan::Shader {
            lease, descriptor, ..
        } = shader;
        let padded = descriptor.output == reel_ui::stage::effect::EffectType::Float
            && source.size != source.storage_size;
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
            context[1][3] = 1.0;
            context[2][3] = opacity;
            context[3][3] = f32::from(nested);
        }
        effect_plan::map_storage(&mut context, source.size, source.storage_size);
        if let Some(mask) = coverage {
            context[9] =
                crate::common::source_view::uv_mapping(self.size, mask.view.size(), [0; 2]);
        }
        if self.scissor.empty() {
            return Ok(output);
        }
        if let Some(mask) = coverage {
            self.ensure_sampled(&mask.view)?;
        }
        if descriptor.source_sampler {
            self.ensure_sampled(&source.pixels.expect("sampled effect source").view)?;
        }
        let overwrite = (!padded
            && terminal.is_none()
            && coverage.is_none()
            && descriptor.full_target_offset.is_some())
        .then(|| {
            crate::common::target_writes::FullOverwrite::composite(
                self.size,
                output.view.size(),
                self.scissor,
                true,
            )
        })
        .flatten();
        let cache = &mut self.effect_cache.0[if overwrite.is_some() {
            4
        } else {
            usize::from(padded) + 2 * usize::from(coverage.is_some())
        }];
        let compiled = if let Some(compiled) = cache.get(
            descriptor.shader_key,
            lease,
            &self.budgets.resources,
            self.limits.resource_bytes,
        )? {
            compiled
        } else {
            let compiled = compile(
                self.gpu,
                &shader,
                &self.budgets.resources,
                self.limits.resource_bytes,
                at,
                padded,
                coverage.is_some(),
                overwrite.is_some(),
            )?;
            cache.insert(
                descriptor.shader_key,
                lease,
                compiled.clone(),
                &self.budgets.resources,
                self.limits.resource_bytes,
            )?;
            compiled
        };
        self.ensure_target(&output)?;
        let mut held = std::mem::take(&mut self.effects);
        let retained = crate::common::scopes::push_stack(&mut held, compiled.clone(), |bytes| {
            self.transient(bytes)
        });
        self.effects = held;
        retained?;
        let image_capacity = effect.images.len()
            + usize::from(descriptor.source_sampler)
            + usize::from(coverage.is_some());
        let metadata_capacity = effect.images.len() * 2;
        let value_capacity =
            effect.values.len() + 3 + effect.images.len() + usize::from(coverage.is_some());
        self.transient(
            (image_capacity * size_of::<&native::Texture>()
                + metadata_capacity * size_of::<[f32; 4]>()
                + value_capacity * size_of::<native::UniformValue<'_>>()) as u64,
        )?;
        let mut images = Vec::with_capacity(image_capacity);
        if descriptor.source_sampler {
            images.push(&source.pixels.expect("sampled effect source").view);
        }
        let mut metadata = Vec::with_capacity(metadata_capacity);
        let mut values = Vec::with_capacity(value_capacity);
        for (index, handle) in effect.images.iter().enumerate() {
            let image = self.images.get(handle).ok_or(GpuError::InvalidRecord {
                at,
                record: "effect texture is not resident",
            })?;
            values.extend(
                compiled.textures[index]
                    .as_ref()
                    .map(|u| native::UniformValue::Int(u, images.len() as i32)),
            );
            images.push(&image.texture);
            metadata.extend(effect_plan::texture_metadata(
                image.layout,
                image.origin,
                image.encoding,
                image.format,
            ));
        }
        if let Some(mask) = coverage {
            let unit = images.len() as i32;
            images.push(&mask.view);
            values.extend(
                compiled
                    .coverage
                    .as_ref()
                    .map(|u| native::UniformValue::Int(u, unit)),
            );
        }
        values.extend(
            compiled
                .texture_meta
                .as_ref()
                .map(|u| native::UniformValue::Vec4Array(u, &metadata)),
        );
        values.extend(
            compiled
                .source
                .as_ref()
                .map(|u| native::UniformValue::Int(u, 0)),
        );
        values.extend(
            compiled
                .context
                .as_ref()
                .map(|u| native::UniformValue::Vec4Array(u, &context)),
        );
        for (uniform, value) in compiled.parameters.iter().zip(&effect.values) {
            values.extend(
                uniform
                    .as_ref()
                    .map(|u| native::UniformValue::Vec4(u, effect_plan::slot(*value))),
            );
        }
        let mut index_count = 0;
        let mesh = if padded {
            let geometry =
                effect_plan::implicit_mesh::geometry(&context).ok_or(GpuError::InvalidSize)?;
            let mut mesh = crate::common::mesh::Mesh::new(self.mesh_allocation());
            mesh.indexed_triangles(geometry.vertices(), geometry.indices().iter().copied())?;
            index_count = geometry.indices().len() as u32;
            let batch = crate::common::draw_batch::Batch::new(mesh, ())?;
            let (storage, local, metadata) = super::geometry::upload_batch(
                self.geometry_cache,
                self.gpu,
                self.budgets,
                &self.budget_scope,
                self.limits,
                &batch,
            )?;
            self.tickets.push(crate::common::renderer::Reservation {
                _shared: None,
                _local: local,
            });
            if let Some(local) = metadata {
                self.tickets.push(crate::common::renderer::Reservation {
                    _shared: None,
                    _local: local,
                });
            }
            let mut held = std::mem::take(&mut self.buffers);
            let result = crate::common::scopes::push_stack(&mut held, storage, |bytes| {
                self.tickets.push(crate::common::renderer::reserve(
                    &self.budgets.transient,
                    &self.budget_scope.transient,
                    bytes,
                    self.limits.transient_bytes,
                    "effect mesh metadata",
                )?);
                Ok(())
            });
            self.buffers = held;
            result?;
            Some(&self.buffers.last().unwrap().storage)
        } else {
            None
        };
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::EffectComposite);
        let _draw = reel_core::profile::Scope::new(reel_core::profile::Phase::Draw);
        #[cfg(feature = "profiling")]
        let submit_start = std::time::Instant::now();
        self.gpu.draw(native::Draw {
            target: native::Target::Framebuffer(&output.framebuffer),
            size: self.size,
            program: &compiled.program,
            vertices: mesh.map_or(&self.programs.vertices, |mesh| &mesh.vertices),
            instances: None,
            indices: Some(mesh.map_or(&self.programs.indices, |mesh| &mesh.indices)),
            range: 0..if padded { index_count } else { 6 },
            textures: &images,
            uniforms: &values,
            blend: if terminal.is_some() {
                native::Blend::Premultiplied
            } else {
                native::Blend::Replace
            },
            scissor: self.native_scissor(),
        })?;
        #[cfg(feature = "profiling")]
        {
            use crate::profiling::{EffectSample, area, record_effect};
            record_effect(EffectSample {
                definition: effect.definition,
                variant: if overwrite.is_some() {
                    2
                } else {
                    u8::from(coverage.is_some())
                },
                draws: 1,
                source_pixels: if descriptor.source_sampler {
                    area(source.size)
                } else {
                    0
                },
                source_storage_pixels: if descriptor.source_sampler {
                    area(source.storage_size)
                } else {
                    0
                },
                output_pixels: area(self.size),
                output_storage_pixels: area(output.view.size()),
                scissor_pixels: area([self.scissor.width, self.scissor.height]),
                sampler_bindings: images.len() as u64,
                submit_wall_us: submit_start.elapsed().as_micros() as u64,
                max_source: if descriptor.source_sampler {
                    source.storage_size
                } else {
                    [0; 2]
                },
                max_output: output.view.size(),
                largest_owner: at.owner.to_raw(),
                ..Default::default()
            });
        }
        Ok(output)
    }
}
