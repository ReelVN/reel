use super::*;

impl Context<'_> {
    fn copy_backdrop(
        &mut self,
        target: &Target,
        source: &Target,
        read: Scissor,
        write: Scissor,
    ) -> Result<bool, GpuError> {
        if target.view.format() != native::TextureFormat::Rgba8
            || source.view.format() != native::TextureFormat::Rgba8
            || target.view.target_storage() != Some(native::TargetStorage::Linear)
            || source.view.target_storage() != Some(native::TargetStorage::Linear)
            || target.same_allocation(source)
        {
            return Ok(false);
        }
        let Some(region) = crate::common::backdrop_copy::Region::new(
            source.view.size(),
            target.view.size(),
            read,
            write,
            1,
        ) else {
            return Ok(false);
        };
        if !self.target_writes.pending(target.slot)
            || !region.overwrites_destination(target.view.size())
        {
            // Keep clear and partial draw in one scene instead of storing a
            // complete clear before transferring the same pixels again. Only
            // a fresh target makes the replaced source-over a pure pixel copy.
            return Ok(false);
        }
        self.ensure_sampled(&source.view)?;
        self.target_writes.take(target.slot);
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::TextureCopy);
        self.gpu.copy_texture(
            &source.view,
            region.source_origin,
            &target.view,
            region.destination_origin,
            region.extent,
        )?;
        Ok(true)
    }

    pub fn combine(
        &mut self,
        target: &Target,
        source: &Target,
        mask: &Target,
        alpha: f32,
        invert: f32,
        operation: CompositeOperation,
    ) -> Result<(), GpuError> {
        self.composition(
            target,
            Composition {
                source,
                mask,
                backdrop: source,
                backdrop_origin: [0; 2],
                alpha,
                invert,
                operation,
                blend: 0,
                expression: None,
                source_origin: [0; 2],
            },
        )
    }
    pub(super) fn composition(
        &mut self,
        target: &Target,
        composition: Composition<'_>,
    ) -> Result<(), GpuError> {
        self.flush_draws()?;
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Composite);
        if self.scissor.empty() {
            return Ok(());
        }
        let Composition {
            source,
            mask,
            backdrop,
            backdrop_origin,
            alpha,
            invert,
            operation,
            blend,
            expression,
            source_origin,
        } = composition;
        let (mode, progress) = match operation {
            CompositeOperation::SourceOver => (0., 0.),
            CompositeOperation::AlphaMask => (1., 0.),
            CompositeOperation::LuminanceMask => (2., 0.),
            CompositeOperation::Transition(p) => (3., p),
            CompositeOperation::MaskedTransition(p) => (4., p),
        };
        let plain = mode == 0. && invert == 0. && blend == 0 && expression.is_none();
        let coverage = mode == 1. && invert == 0. && blend == 0 && expression.is_none();
        let current_pixel = self.offscreen.destination().current_pixel_code(
            blend,
            matches!(operation, CompositeOperation::SourceOver),
            expression.is_some(),
        );
        let replacing = matches!(
            operation,
            CompositeOperation::Transition(_) | CompositeOperation::MaskedTransition(_)
        ) || blend != 0;
        let overwrite = crate::common::target_writes::FullOverwrite::composite(
            self.size,
            target.view.size(),
            self.scissor,
            replacing && !current_pixel,
        );
        let _kind = reel_core::profile::Scope::new(if plain {
            reel_core::profile::Phase::SourceComposite
        } else if coverage {
            reel_core::profile::Phase::CoverageComposite
        } else {
            reel_core::profile::Phase::EffectComposite
        });
        let shader = if invert == 0.
            && blend == 0
            && matches!(
                operation,
                CompositeOperation::Transition(_) | CompositeOperation::MaskedTransition(_)
            )
            && (!matches!(operation, CompositeOperation::MaskedTransition(_))
                || expression.is_some())
        {
            self.programs.transition(
                self.gpu,
                matches!(operation, CompositeOperation::MaskedTransition(_)),
            )?
        } else if plain {
            &self.programs.source_over
        } else if coverage {
            &self.programs.alpha_mask
        } else if blend > 4 {
            &self.programs.extended_blend
        } else {
            &self.programs.composite
        };
        #[cfg(feature = "gxm-tile-destination")]
        let shader = if current_pixel {
            self.programs.tile(self.gpu, blend, invert)?
        } else {
            shader
        };
        self.ensure_sampled(&source.view)?;
        if !plain && !current_pixel {
            self.ensure_sampled(&mask.view)?;
        }
        if blend != 0 && !current_pixel {
            self.ensure_sampled(&backdrop.view)?;
        }
        if let Some(expression) = expression {
            self.ensure_sampled(&expression.view)?;
        }
        if overwrite.is_some() {
            self.target_writes.take(target.slot);
        } else {
            self.ensure_target(target)?;
        }
        let mut values = Values::new();
        for (name, unit) in [
            ("u_source", 0),
            ("u_mask", 1),
            ("u_backdrop", 2),
            ("u_expression", 3),
        ] {
            values.extend(shader.int(name, unit));
        }
        values.extend(shader.vec4("u_params", [alpha, mode, progress, 0.]));
        values.extend(shader.vec4("u_source_uv", source.source_uv(self.size, source_origin)));
        for (name, input) in [
            ("u_mask_uv", mask),
            ("u_expression_uv", expression.unwrap_or(source)),
        ] {
            values.extend(shader.vec4(name, input.frame_uv(self.size)));
        }
        values.extend(shader.vec4(
            "u_backdrop_uv",
            backdrop.source_uv(self.size, backdrop_origin),
        ));
        values.extend(shader.float("u_invert", invert));
        values.extend(shader.float("u_blend", blend as f32));
        values.extend(shader.float("u_has_expression", f32::from(expression.is_some())));
        self.programs.quad(
            self.gpu,
            programs::Quad {
                target: native::Target::Framebuffer(&target.framebuffer),
                size: self.size,
                shader,
                textures: &[
                    &source.view,
                    &mask.view,
                    &backdrop.view,
                    &expression.unwrap_or(source).view,
                ][..if plain || current_pixel {
                    1
                } else if coverage {
                    2
                } else {
                    4
                }],
                values: &values,
                blend: if replacing {
                    native::Blend::Replace
                } else {
                    native::Blend::Premultiplied
                },
                scissor: self.native_scissor(),
            },
        )
    }

    pub(super) fn combine_cropped(
        &mut self,
        target: &Target,
        source: &Target,
        view: crate::common::source_view::SourceView,
        composite: reel_ui::frame::GroupComposite,
        invert: f32,
    ) -> Result<(), GpuError> {
        self.flush_draws()?;
        if self.scissor.empty() {
            return Ok(());
        }
        if self
            .offscreen
            .destination()
            .current_pixel(composite.blend, true, false)
        {
            return self.composition(
                target,
                Composition {
                    source,
                    mask: source,
                    backdrop: source,
                    backdrop_origin: [0; 2],
                    alpha: composite.alpha,
                    invert,
                    operation: CompositeOperation::SourceOver,
                    blend: blend_code(composite.blend).expect("validated group blend"),
                    expression: None,
                    source_origin: view.origin(),
                },
            );
        }
        // Allocation capacity follows the common plan; the copy keeps the source
        // grid and initializes only the pixels sampled by the final blend.
        let size = self.size;
        let scissor = self.scissor;
        self.size = self.offscreen.backdrop_size(view.storage_size, size);
        let mut backdrop = self.target()?;
        backdrop.size = view.storage_size;
        self.size = view.storage_size;
        self.scissor = view.source_rect;
        if !self.copy_backdrop(&backdrop, target, view.parent_rect, view.source_rect)? {
            self.composition(
                &backdrop,
                Composition {
                    source: target,
                    mask: target,
                    backdrop: target,
                    backdrop_origin: [0; 2],
                    alpha: 1.,
                    invert: 0.,
                    operation: CompositeOperation::SourceOver,
                    blend: 0,
                    expression: None,
                    source_origin: view.origin().map(|coordinate| -coordinate),
                },
            )?;
        }
        self.size = size;
        self.scissor = scissor;
        self.composition(
            target,
            Composition {
                source,
                mask: source,
                backdrop: &backdrop,
                backdrop_origin: view.origin(),
                alpha: composite.alpha,
                invert,
                operation: CompositeOperation::SourceOver,
                blend: blend_code(composite.blend).expect("validated group blend"),
                expression: None,
                source_origin: view.origin(),
            },
        )?;
        self.recycle_target(backdrop)
    }

    pub(super) fn combine_group(
        &mut self,
        target: &Target,
        source: &Target,
        mask: &Target,
        paint: GroupPaint<'_>,
    ) -> Result<(), GpuError> {
        self.flush_draws()?;
        if self.scissor.empty() {
            return Ok(());
        }
        let GroupPaint {
            source_origin,
            composite,
            invert,
            operation,
            image_mask,
        } = paint;
        let blend = blend_code(composite.blend)
            .ok_or(GpuError::Device("unsupported group blend".into()))?;
        if blend == 0
            && invert == 0.
            && matches!(operation, CompositeOperation::SourceOver)
            && let Some((definition, transform)) = image_mask
            && self.composite_sample_mask(target, source, definition, transform, composite.alpha)?
        {
            return Ok(());
        }
        let expression = if let Some((definition, transform)) = image_mask {
            Some(self.mask_expression(definition, transform)?)
        } else {
            None
        };
        let backdrop = if self.offscreen.destination().backdrop(
            composite.blend,
            matches!(operation, CompositeOperation::SourceOver),
            image_mask.is_some(),
        ) {
            let backdrop = self.target()?;
            if !self.copy_backdrop(&backdrop, target, self.scissor, self.scissor)? {
                self.combine(
                    &backdrop,
                    target,
                    target,
                    1.,
                    0.,
                    CompositeOperation::SourceOver,
                )?;
            }
            Some(backdrop)
        } else {
            None
        };
        self.composition(
            target,
            Composition {
                source,
                mask,
                backdrop: backdrop.as_ref().unwrap_or(source),
                backdrop_origin: [0; 2],
                alpha: composite.alpha,
                invert,
                operation,
                blend,
                expression: expression.as_ref(),
                source_origin,
            },
        )?;
        if let Some(backdrop) = backdrop {
            self.recycle_target(backdrop)?;
        }
        if let Some(expression) = expression {
            self.recycle_target(expression)?;
        }
        Ok(())
    }
    pub(super) fn combine_transition(
        &mut self,
        target: &Target,
        old: &Target,
        new: &Target,
        transition: &SceneTransition,
        outer: Affine,
    ) -> Result<(), GpuError> {
        let expression = if let Some(definition) = transition
            .mask
            .as_ref()
            .filter(|_| transition.progress > 0. && transition.progress < 1.)
        {
            Some(self.mask_expression(definition, outer * transition.mask_to_window)?)
        } else {
            None
        };
        let operation = if expression.is_some() {
            CompositeOperation::MaskedTransition(transition.progress)
        } else {
            CompositeOperation::Transition(transition.progress)
        };
        self.composition(
            target,
            Composition {
                source: old,
                mask: new,
                backdrop: old,
                backdrop_origin: [0; 2],
                alpha: 1.,
                invert: 0.,
                operation,
                blend: 0,
                expression: expression.as_ref(),
                source_origin: [0; 2],
            },
        )?;
        if let Some(expression) = expression {
            self.recycle_target(expression)?;
        }
        Ok(())
    }
}
