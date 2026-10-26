//! Prepared asset programs; Vita consumes only exported SGX binaries.
use super::{GlesError, native, programs, replay::Context, resources::Target};
use crate::common::{effect_cache, effect_plan};
use reel_core::budget::{BudgetPool, BudgetTicket};
use reel_ui::{
    frame::{FrameDrawing, FrameEffect, RecordLocation},
    kurbo::Affine,
    resources::EffectShaderFormat,
};
use std::rc::Rc;

pub(super) type Cache = effect_cache::Cache<Rc<Compiled>>;
pub(super) struct Compiled {
    program: native::Program,
    source: Option<native::Uniform>,
    context: Option<native::Uniform>,
    parameters: Vec<Option<native::Uniform>>,
    _storage: BudgetTicket,
}

fn compile(
    gpu: &native::Context,
    shader: &effect_plan::Shader<'_>,
    budget: &BudgetPool,
    limit: u64,
    at: RecordLocation,
) -> Result<Rc<Compiled>, GlesError> {
    let effect_plan::Shader {
        lease,
        descriptor,
        bytes,
    } = *shader;
    let count = descriptor.parameters.len();
    let format = descriptor.format;
    if count.saturating_add(crate::effect::CONTEXT_SLOTS)
        > gpu.capabilities().max_fragment_uniform_vectors as usize
    {
        return Err(GlesError::Unsupported {
            at,
            record: "effect uniforms exceed the device capacity",
        });
    }
    #[cfg(target_os = "vita")]
    let code = if format == EffectShaderFormat::Sgx543 {
        native::ShaderCode::SgxBinary(bytes)
    } else {
        return Err(GlesError::Unsupported {
            at,
            record: "Vita requires an exported SGX effect shader",
        });
    };
    #[cfg(not(target_os = "vita"))]
    let code = if format == EffectShaderFormat::GlslEs100 {
        native::ShaderCode::Source(std::str::from_utf8(bytes).map_err(|_| {
            GlesError::InvalidRecord {
                at,
                record: "effect GLSL encoding",
            }
        })?)
    } else {
        return Err(GlesError::Unsupported {
            at,
            record: "GLES requires a prepared GLSL effect shader",
        });
    };
    let storage = effect_cache::reserve(
        budget,
        size_of::<Compiled>()
            + 2 * size_of::<usize>()
            + count * size_of::<Option<native::Uniform>>(),
        limit,
    )?;
    let program = programs::create_program(gpu, code).map_err(|error| GlesError::Shader {
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
        program,
        source,
        context,
        parameters,
        _storage: storage,
    }))
}

impl Context<'_> {
    pub(super) fn effect(
        &mut self,
        drawing: &FrameDrawing,
        effect: &FrameEffect,
        transform: Affine,
        source: &Target,
        at: RecordLocation,
    ) -> Result<Target, GlesError> {
        self.flush_draws()?;
        let shader = effect_plan::shader(drawing, effect, at)?;
        let effect_plan::Shader {
            lease, descriptor, ..
        } = shader;
        let compiled = if let Some(compiled) = self.effect_cache.get(
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
            )?;
            self.effect_cache.insert(
                descriptor.shader_key,
                lease,
                compiled.clone(),
                &self.budgets.resources,
                self.limits.resource_bytes,
            )?;
            compiled
        };
        let output = self.target()?;
        let Some(context) = effect_plan::context(effect, transform, self.size, source.size) else {
            return Ok(output);
        };
        if self.scissor.empty() {
            return Ok(output);
        }
        self.transient(((effect.values.len() + 2) * size_of::<native::UniformValue<'_>>()) as u64)?;
        let mut values = Vec::with_capacity(effect.values.len() + 2);
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
        let mut held = std::mem::take(&mut self.effects);
        let retained = crate::common::scopes::push_stack(&mut held, compiled.clone(), |bytes| {
            self.transient(bytes)
        });
        self.effects = held;
        retained?;
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::EffectComposite);
        let _draw = reel_core::profile::Scope::new(reel_core::profile::Phase::Draw);
        self.gpu.draw(native::Draw {
            target: native::Target::Framebuffer(&output.framebuffer),
            size: self.size,
            program: &compiled.program,
            vertices: &self.programs.vertices,
            indices: Some(&self.programs.indices),
            range: 0..6,
            textures: &[&source.view],
            uniforms: &values,
            blend: native::Blend::Replace,
            scissor: Some([
                self.scissor.x,
                self.scissor.y,
                self.scissor.width,
                self.scissor.height,
            ]),
        })?;
        Ok(output)
    }
}
