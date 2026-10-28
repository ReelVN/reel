//! Ordered instance runs retain material state and completion-owned storage.
use super::*;
use crate::common::quad_instances::{self, Batch, Input};
pub(in crate::gxm) struct InstanceBatch {
    target: Target,
    geometry: Batch,
    state: PaintState,
    size: [u32; 2],
    clip: crate::common::scissor::Scissor,
}
impl<'a> Paint<'a> {
    pub(super) fn image(
        image: &'a Image,
        repeat: [bool; 2],
        appearance: Option<ImageAppearance>,
        quality: ImageQuality,
    ) -> Option<Self> {
        if appearance
            .and_then(|a| a.gradient)
            .is_some_and(|g| g.transform.determinant() == 0.)
        {
            return None;
        }
        let mut values = appearance.map(appearance_uniform).unwrap_or([[0.; 4]; 9]);
        values[8] = [
            image.layout.content_size[0] as f32 / image.layout.storage_size[0] as f32,
            image.layout.content_size[1] as f32 / image.layout.storage_size[1] as f32,
            match image.format {
                Some(PixelFormat::Bc1Rgb) => 1.,
                Some(PixelFormat::Bc3Rgba) => 2.,
                _ => 0.,
            },
            f32::from(quality == ImageQuality::Low),
        ];
        if values[4][3] == 3. && image.kind == 0. && image.encoding == ColorEncoding::Linear {
            values[4][3] = 4.;
        }
        Some(Self {
            texture: &image.texture,
            planes: &image.planes,
            kind: image.kind,
            video: &image.video,
            dimensions: image.layout.storage_size,
            appearance: values,
            repeat,
            coverage: image.coverage,
            format: image.format,
            quality,
            source: mesh::PaintSource::Texture,
        })
    }
    pub(super) fn state(
        self,
        direct: Option<mesh::DirectPaint>,
        bounded: bool,
        opaque: Option<mesh::OpaquePaint>,
        scissor: crate::common::scissor::Scissor,
    ) -> PaintState {
        PaintState {
            texture: self.texture.clone(),
            planes: self.planes.iter().cloned().collect(),
            kind: self.kind,
            video: *self.video,
            dimensions: self.dimensions,
            appearance: self.appearance,
            repeat: self.repeat,
            quality: self.quality,
            coverage: self.coverage,
            direct,
            bounded,
            opaque,
            scissor,
        }
    }
}
impl PaintState {
    fn same_instance_paint(&self, other: &Self) -> bool {
        self.texture == other.texture
            && self.planes == other.planes
            && self.kind == other.kind
            && self.video == other.video
            && self.dimensions == other.dimensions
            && self.appearance == other.appearance
            && self.repeat == other.repeat
            && self.quality == other.quality
            && self.coverage == other.coverage
            && self.direct == other.direct
            && self.bounded == other.bounded
            && self.opaque == other.opaque
    }
    pub(super) fn shader_kind(&self) -> super::super::programs::PaintKind {
        use super::super::programs::PaintKind;
        let styled = self.appearance[5][3] != 0. || self.appearance[7][3] != 0.;
        if let Some(direct) = self.direct {
            if self.bounded {
                PaintKind::Bounded(direct)
            } else {
                PaintKind::Direct(direct)
            }
        } else if self.kind > 0. {
            if styled {
                PaintKind::VideoAppearance
            } else {
                PaintKind::Video
            }
        } else if self.coverage {
            if self.appearance[4][3] == 2. {
                PaintKind::GlyphGradient
            } else {
                PaintKind::Coverage
            }
        } else if self.appearance[4][3] >= 3. {
            PaintKind::Packed
        } else if self.appearance[4][3] == 2. {
            PaintKind::Gradient
        } else if styled {
            PaintKind::Appearance
        } else {
            PaintKind::Rgba
        }
    }
}
impl Context<'_> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn sprite_instances(
        &mut self,
        target: &Target,
        batch: &reel_ui::frame::FrameSprites,
        outer: Affine,
        image: &Image,
        coverage: Option<&reel_core::image_coverage::ImageCoverage>,
        at: RecordLocation,
    ) -> Result<bool, GpuError> {
        if self.mesh_clip.is_some()
            || image.kind != 0.
            || target.view.format() == native::TextureFormat::Coverage8
            || batch.instances.len() < quad_instances::MIN_INSTANCES
                && self.instance_batch.is_none()
        {
            return Ok(false);
        }
        let Some(input) = Input::sprites(
            batch,
            outer,
            image.layout,
            image.uv_transform(),
            coverage,
            self.size,
            at,
        )?
        else {
            return Ok(false);
        };
        if self.instance_batch.as_ref().is_some_and(|pending| {
            pending.state.texture != image.texture
                || !pending.geometry.template.same(&input.template)
        }) {
            self.expand_instances()?;
        }
        let geometry = Batch::new(&input, self.mesh_allocation())?;
        if geometry.instances.is_empty() {
            return Ok(true);
        }
        let repeat = [
            batch.brush.sampler.x_extend == Extend::Repeat,
            batch.brush.sampler.y_extend == Extend::Repeat,
        ];
        let Some(paint) = Paint::image(
            image,
            repeat,
            batch.brush.image.appearance.map(|a| a.transformed(outer)),
            batch.brush.sampler.quality,
        ) else {
            return Ok(true);
        };
        let direct = if paint.coverage {
            geometry.direct_coverage(Some(&paint.appearance))
        } else {
            geometry.direct_paint(paint.source, repeat, Some(&paint.appearance), paint.format)
        };
        let bounded = geometry.bounded_color.paint(direct, paint.format);
        let state = paint.state(
            direct,
            bounded,
            geometry.opaque_paint(direct),
            geometry.scissor(self.scissor, self.size),
        );
        if let Some(mut pending) = self.instance_batch.take() {
            if pending.state.same_instance_paint(&state)
                && pending.clip == self.scissor
                && pending.geometry.template.same(&geometry.template)
                && pending.target.slot == target.slot
                && pending.size == self.size
            {
                if pending.geometry.can_merge(&geometry) {
                    pending.geometry.merge(geometry)?;
                    pending.state.scissor = pending.geometry.scissor(pending.clip, pending.size);
                    self.instance_batch = Some(pending);
                    return Ok(true);
                }
                self.instance_batch = Some(pending);
                self.finish_instances()?;
            } else {
                self.instance_batch = Some(pending);
                self.expand_instances()?;
            }
        }
        let before = self.paint_batch.as_ref().is_some_and(|batch| {
            batch.target.slot == target.slot
                && !batch.geometry.full_bytes(geometry.expanded_bytes())
        });
        if !geometry.instanced(before, false) {
            self.queue_mesh(target, geometry.into_mesh()?.into(), state)?;
            return Ok(true);
        }
        self.instance_batch = Some(InstanceBatch {
            target: target.clone(),
            geometry,
            state,
            size: self.size,
            clip: self.scissor,
        });
        Ok(true)
    }
    pub(super) fn image_instances(&mut self, _image: &Image) -> Result<(), GpuError> {
        self.expand_instances()
    }
    pub(super) fn expand_instances(&mut self) -> Result<(), GpuError> {
        if let Some(pending) = self.instance_batch.take() {
            self.queue_mesh(
                &pending.target,
                pending.geometry.into_mesh()?.into(),
                pending.state,
            )?;
        }
        Ok(())
    }
    pub(super) fn finish_instances(&mut self) -> Result<(), GpuError> {
        let Some(pending) = self.instance_batch.take() else {
            return Ok(());
        };
        let before = self.paint_batch.as_ref().is_some_and(|batch| {
            batch.target.slot == pending.target.slot
                && !batch.geometry.full_bytes(pending.geometry.expanded_bytes())
        });
        if !pending.geometry.instanced(before, false) {
            return self.queue_mesh(
                &pending.target,
                pending.geometry.into_mesh()?.into(),
                pending.state,
            );
        }
        self.flush_draws()?;
        self.submit_instances(pending)
    }
    fn submit_instances(&mut self, pending: InstanceBatch) -> Result<(), GpuError> {
        self.ensure_target(&pending.target)?;
        let (storage, local) = super::super::geometry::upload_instances(
            self.geometry_cache,
            self.gpu,
            self.budgets,
            &self.budget_scope,
            self.limits,
            &pending.geometry,
        )?;
        self.tickets.push(crate::common::renderer::Reservation {
            _shared: None,
            _local: local,
        });
        self.buffers.push(storage);
        let storage = &self.buffers.last().unwrap().storage;
        let state = &pending.state;
        let shader = self.programs.paint.get(
            self.gpu,
            state.shader_kind(),
            !state.scissor.tile_exact(32, pending.target.view.size()),
            true,
        )?;
        let mut values = Values::new();
        for (name, unit) in [("u_image", 0), ("u_chroma_u", 1), ("u_chroma_v", 2)] {
            values.extend(shader.int(name, unit));
        }
        values.extend(shader.vec2("u_dimensions", state.dimensions.map(|n| n as f32)));
        values.extend(shader.vec2("u_output_size", pending.size.map(|n| n as f32)));
        values.extend(shader.vec2("u_repeat", state.repeat.map(f32::from)));
        values.extend(shader.array("u_appearance[0]", &state.appearance));
        values.extend(shader.array("u_video[0]", &state.video));
        values.extend(shader.float("u_video_kind", state.kind));
        values.extend(shader.float("u_image_flip", 0.));
        values.extend(shader.float("u_coverage", f32::from(state.coverage)));
        self.gpu.set_repeat(&state.texture, state.repeat)?;
        let textures = [
            &state.texture,
            state.planes.first().unwrap_or(&state.texture),
            state
                .planes
                .get(1)
                .or(state.planes.first())
                .unwrap_or(&state.texture),
        ];
        for start in (0..pending.geometry.instances.len()).step_by(quad_instances::MAX_INSTANCES) {
            let end = (start + quad_instances::MAX_INSTANCES).min(pending.geometry.instances.len());
            let _draw = reel_core::profile::Scope::new(reel_core::profile::Phase::Draw);
            self.gpu.draw_filtered(
                native::Draw {
                    target: native::Target::Framebuffer(&pending.target.framebuffer),
                    size: pending.size,
                    program: &shader.program,
                    vertices: &storage.vertices,
                    instances: Some(native::DrawInstances {
                        vertices: storage.instances.as_deref().unwrap(),
                        range: start as u32..end as u32,
                    }),
                    indices: Some(&storage.indices),
                    range: 0..6,
                    textures: &textures,
                    uniforms: &values,
                    blend: if state.opaque.is_some() {
                        native::Blend::Replace
                    } else {
                        native::Blend::Premultiplied
                    },
                    scissor: Some([
                        state.scissor.x,
                        state.scissor.y,
                        state.scissor.width,
                        state.scissor.height,
                    ]),
                },
                if state.quality == ImageQuality::Low {
                    native::TextureFilter::Nearest
                } else {
                    native::TextureFilter::Linear
                },
            )?;
        }
        Ok(())
    }
}
