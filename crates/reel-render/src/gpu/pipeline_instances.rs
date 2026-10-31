//! Only encountered paint variants compile an instance vertex entry point.
use super::*;
use crate::common::mesh::{DirectPaint, OpaquePaint};

impl Pipelines {
    pub(in crate::gpu) fn instance_pipeline(
        &self,
        gpu: &Gpu,
        quality: Quality,
        direct: Option<DirectPaint>,
        bounded: bool,
        opaque: Option<OpaquePaint>,
    ) -> Result<Arc<wgpu::RenderPipeline>, GpuError> {
        let bounded = bounded && self.bounded.is_some();
        let direct_entries = [
            "fragment_direct",
            "fragment_direct_bc1",
            "fragment_direct_bc3",
            "fragment_regional",
            "fragment_regional_bc1",
            "fragment_regional_bc3",
            "fragment_direct_coverage",
            "fragment_solid",
        ];
        let (key, entry) = if let Some(opaque) = opaque {
            if bounded {
                let index = match opaque {
                    OpaquePaint::Bc1 => 0,
                    OpaquePaint::Solid => 1,
                    OpaquePaint::RegionalBc1 => 2,
                };
                (
                    19 + index,
                    [
                        "fragment_direct_bc1",
                        "fragment_solid",
                        "fragment_regional_bc1",
                    ][index],
                )
            } else {
                let index = opaque as usize;
                (
                    16 + index,
                    [
                        "fragment_direct_bc1",
                        "fragment_regional_bc1",
                        "fragment_solid",
                    ][index],
                )
            }
        } else if let Some(direct) = direct {
            let index = direct as usize;
            if bounded {
                let index = crate::common::precision::paint_index(direct);
                (
                    9 + index,
                    [
                        "fragment_direct",
                        "fragment_direct_bc1",
                        "fragment_direct_bc3",
                        "fragment_solid",
                        "fragment_regional",
                        "fragment_regional_bc1",
                        "fragment_regional_bc3",
                    ][index],
                )
            } else {
                (1 + index, direct_entries[index])
            }
        } else {
            (0, "fragment")
        };
        let mut slots = self
            .instances
            .lock()
            .expect("instance pipeline cache poisoned");
        if let Some(pipeline) = &slots[key] {
            return Ok(pipeline.clone());
        }
        let shader = if bounded {
            include_str!("paint_bounded.wgsl")
        } else {
            include_str!("paint.wgsl")
        };
        let vertex = include_str!("instance_vertex.wgsl")
            .replace(
                "REEL_INSTANCE_COLOR",
                if bounded { "vec4<f16>" } else { "vec4<f32>" },
            )
            .replace("REEL_INSTANCE_WINDOW", "out.source_window = source_window;");
        let code = format!("{shader}\n{vertex}");
        let pipeline = validated(gpu, || {
            Arc::new(pipeline_vertex(
                &gpu.device,
                quality,
                &self.paint_layout,
                &code,
                (VertexInput::Instances, "instance_vertex"),
                FragmentOutput {
                    entry,
                    blend: opaque
                        .is_none()
                        .then_some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                },
                #[cfg(test)]
                None,
            ))
        })?;
        slots[key] = Some(pipeline.clone());
        Ok(pipeline)
    }
}
