//! Exact-position sprite runs retain the ordinary pass and consumer boundaries.
use super::*;
use crate::common::quad_instances::{self, Batch, Input};

pub(in crate::gpu) struct InstanceBatch {
    target: Target,
    geometry: Batch,
    state: PaintState,
    size: [u32; 2],
    clip: super::super::targets::Scissor,
}
impl Context<'_> {
    pub(super) fn sprite_instances(
        &mut self,
        target: &Target,
        batch: &reel_ui::frame::FrameSprites,
        outer: Affine,
        at: RecordLocation,
        image: &resources::Image,
        coverage: Option<&reel_core::image_coverage::ImageCoverage>,
    ) -> Result<bool, GpuError> {
        #[cfg(test)]
        if !self.sprite_instances_enabled {
            return Ok(false);
        }
        if self.mesh_clip.is_some()
            || image.yuv.is_some()
            || matches!(
                target.texture.format(),
                wgpu::TextureFormat::R8Unorm | wgpu::TextureFormat::R16Float
            )
            || (batch.instances.len() < quad_instances::MIN_INSTANCES
                && self.instance_batch.is_none())
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
            image.backing.is_some(),
            at,
        )?
        else {
            return Ok(false);
        };
        if self.instance_batch.as_ref().is_some_and(|pending| {
            pending.state.image != image.view || !pending.geometry.template.same(&input.template)
        }) {
            self.expand_instances()?;
        }
        let geometry = Batch::new(&input, self.mesh_allocation())?;
        if geometry.instances.is_empty() {
            return Ok(true);
        }
        let appearance = batch.brush.image.appearance.map(|a| a.transformed(outer));
        if appearance
            .and_then(|a| a.gradient)
            .is_some_and(|g| g.transform.determinant() == 0.)
        {
            return Ok(true);
        }
        let repeat = [
            batch.brush.sampler.x_extend == Extend::Repeat,
            batch.brush.sampler.y_extend == Extend::Repeat,
        ];
        let appearance = image_appearance(image, appearance, batch.brush.sampler.quality, repeat);
        let direct = geometry.direct_paint(
            mesh::PaintSource::Texture,
            repeat,
            appearance.as_ref(),
            image.texture_format,
        );
        let state = PaintState {
            image: image.view.clone(),
            texture_format: image.texture_format,
            yuv: None,
            coverage: false,
            repeat,
            quality: batch.brush.sampler.quality,
            appearance: if direct.is_some() { None } else { appearance },
            direct,
            bounded: geometry.bounded_color.paint(direct, image.texture_format),
            opaque: geometry.opaque_paint(direct),
            scissor: geometry.scissor(self.scissor, self.size),
        };
        if let Some(mut pending) = self.instance_batch.take() {
            if pending.state.same_material(&state)
                && pending.state.direct == state.direct
                && pending.state.bounded == state.bounded
                && pending.state.opaque == state.opaque
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
        let occupied = self.paint_batch.as_ref().is_some_and(|batch| {
            batch.target.slot == target.slot
                && !batch.geometry.full_bytes(geometry.expanded_bytes())
        });
        if !geometry.instanced(occupied, false) {
            self.queue_mesh(target, geometry.into_mesh()?.into(), state)?;
        } else {
            self.instance_batch = Some(InstanceBatch {
                target: target.clone(),
                geometry,
                state,
                size: self.size,
                clip: self.scissor,
            });
        }
        Ok(true)
    }
    pub(in crate::gpu) fn expand_instances(&mut self) -> Result<(), GpuError> {
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
        let occupied = self.paint_batch.as_ref().is_some_and(|batch| {
            batch.target.slot == pending.target.slot
                && !batch.geometry.full_bytes(pending.geometry.expanded_bytes())
        });
        if !pending.geometry.instanced(occupied, false) {
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
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::DrawPrepare);
        let pipeline = self.pipelines.instance_pipeline(
            self.gpu,
            self.quality,
            pending.state.direct,
            pending.state.bounded,
            pending.state.opaque,
        )?;
        self.transient(
            (size_of::<wgpu::BindGroup>()
                + if pending.state.appearance.is_some() {
                    size_of::<crate::common::gradient::Uniform>()
                } else {
                    0
                }) as u64,
        )?;
        let group = self.paint_group(&pending.state);
        let (storage, local) = self.geometry_cache.upload_instances(
            self.gpu,
            self.budgets,
            &self.budget_scope,
            &pending.geometry,
            self.geometry_policy,
            &mut self.encoder,
        )?;
        self.tickets.push(super::super::renderer::Reservation {
            _shared: None,
            _local: local,
        });
        self.geometry.push(storage);
        let storage = &self.geometry.last().unwrap().storage;
        let mut pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("reel ordered sprite instances"),
            color_attachments: &[Some(
                pending
                    .target
                    .attachment(self.target_writes.take(pending.target.slot)),
            )],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            multiview_mask: None,
            occlusion_query_set: None,
        });
        pass.set_viewport(
            0.,
            0.,
            pending.target.size[0] as f32,
            pending.target.size[1] as f32,
            0.,
            1.,
        );
        pass.set_pipeline(&pipeline);
        pending.state.scissor.apply(&mut pass);
        pass.set_bind_group(0, &group, &[]);
        pass.set_vertex_buffer(0, storage.vertices.slice(..));
        pass.set_vertex_buffer(1, storage.instances.as_ref().unwrap().slice(..));
        pass.set_index_buffer(storage.indices.slice(..), wgpu::IndexFormat::Uint16);
        for start in (0..pending.geometry.instances.len()).step_by(quad_instances::MAX_INSTANCES) {
            let end = (start + quad_instances::MAX_INSTANCES).min(pending.geometry.instances.len());
            let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Draw);
            #[cfg(test)]
            self.gpu
                .upload_creations
                .draws
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            pass.draw_indexed(0..6, 0, start as u32..end as u32);
        }
        Ok(())
    }
}
