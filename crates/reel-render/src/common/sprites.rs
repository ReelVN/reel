//! Both backends expand the same ordered sprite stream into common image quads.
use super::{
    GpuError,
    mesh::{ImagePaint, ImageSampling, Mesh},
};
use reel_core::{image_coverage::ImageCoverage, texture::TextureLayout};
use reel_ui::{
    frame::{FrameSprites, RecordLocation},
    kurbo::{Affine, Rect},
    peniko::{Extend, ImageQuality},
};
impl Mesh {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sprites(
        &mut self,
        batch: &FrameSprites,
        outer: Affine,
        layout: TextureLayout,
        uv: Affine,
        coverage: Option<&ImageCoverage>,
        size: [u32; 2],
        at: RecordLocation,
    ) -> Result<(), GpuError> {
        if batch.brush.sampler.quality == ImageQuality::High
            || batch.brush.sampler.x_extend == Extend::Reflect
            || batch.brush.sampler.y_extend == Extend::Reflect
        {
            return Err(GpuError::Unsupported {
                at,
                record: "sprite image sampling",
            });
        }
        let repeat = [
            batch.brush.sampler.x_extend == Extend::Repeat,
            batch.brush.sampler.y_extend == Extend::Repeat,
        ];
        let [_, _, _, _, x, y] = uv.as_coeffs();
        let sampling =
            ImageSampling::with_origin(batch.brush.image.source_rect, layout, [x, y], repeat);
        let outer = outer * batch.transform;
        let viewport = Rect::new(0., 0., f64::from(size[0]), f64::from(size[1]));
        for instance in &batch.instances {
            if instance.alpha <= 0. || !instance.has_area() {
                continue;
            }
            let transform = outer * instance.transform(batch.bounds);
            let visible = transform
                .transform_rect_bbox(batch.bounds)
                .inflate(1., 1.)
                .intersect(viewport);
            if visible.width() <= 0. || visible.height() <= 0. {
                continue;
            }
            let alpha = instance.alpha * batch.composite.alpha * batch.brush.sampler.alpha;
            let color = batch.brush.image.color.map(|c| c * alpha);
            for bounds in super::image_coverage::quads(
                batch.bounds,
                &batch.brush,
                Affine::IDENTITY,
                transform,
                layout,
                coverage,
            )
            .iter()
            {
                self.image_quad(
                    bounds,
                    transform,
                    uv,
                    size,
                    ImagePaint { color, sampling },
                    at,
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{
        GpuLimits,
        mesh::{Allocation, RectClip},
        renderer::Budgets,
    };
    use reel_ui::core::Widget;
    use reel_ui::{
        frame::{FrameImage, SpriteInstance},
        imaging::{
            Composite,
            record::{Clip, Geometry},
        },
        peniko::{ImageBrush, ImageSampler},
        resources::ResourceHandle,
    };
    fn batch() -> FrameSprites {
        FrameSprites {
            transform: Affine::new([1.2, 0.1, 0.2, 0.8, 30., 40.]),
            bounds: Rect::new(2., 1., 14., 7.),
            brush: ImageBrush {
                image: FrameImage {
                    resource: ResourceHandle {
                        registry: 1,
                        slot: 0,
                        generation: 1,
                    },
                    color: [0.4, 0.3, 0.2, 0.5],
                    appearance: None,
                    source_rect: Some(Rect::new(2., 1., 14., 7.)),
                },
                sampler: ImageSampler {
                    alpha: 0.75,
                    ..Default::default()
                },
            },
            composite: Composite {
                alpha: 0.5,
                ..Default::default()
            },
            instances: vec![
                SpriteInstance {
                    position: [10., 20.],
                    size: 24.,
                    rotation: 0.4,
                    alpha: 0.2,
                },
                SpriteInstance {
                    position: [22., 21.],
                    size: 32.,
                    rotation: -0.7,
                    alpha: 0.8,
                },
                SpriteInstance {
                    position: [10000.; 2],
                    size: 16.,
                    rotation: 0.,
                    alpha: 1.,
                },
            ]
            .into_iter()
            .map(|s| s.compiled(Rect::new(2., 1., 14., 7.)))
            .collect(),
        }
    }
    #[test]
    fn sprite_mesh_preserves_original_quad_order_sampling_alpha_and_clipping() {
        let limits = GpuLimits::default();
        let global = Budgets::new(limits);
        let local = Budgets::new(limits);
        let allocation = || Allocation::new(&global, &local, limits.transient_bytes, 1 << 20);
        let batch = batch();
        let outer = Affine::new([-1., 0., 0., 1., 150., 7.]);
        let size = [200, 200];
        let layout = TextureLayout {
            logical_size: [16, 8],
            content_size: [16, 8],
            storage_size: [32, 16],
        };
        let uv = Affine::scale_non_uniform(1. / 32., 1. / 16.);
        let at = RecordLocation {
            fragment: 0,
            command: 0,
            mask_depth: 0,
            owner: reel_ui::widgets::Label::new("").prepare().id(),
        };
        let mut actual = Mesh::new(allocation());
        actual
            .sprites(&batch, outer, layout, uv, None, size, at)
            .unwrap();
        let mut reference = Mesh::new(allocation());
        for (s, (sprite_size, rotation)) in
            batch.instances[..2].iter().zip([(24., 0.4), (32., -0.7)])
        {
            let transform = outer
                * batch.transform
                * Affine::translate((s.position[0], s.position[1]))
                * Affine::rotate(rotation)
                * Affine::scale(sprite_size / 12.)
                * Affine::translate((-8., -4.));
            reference
                .image_quad(
                    batch.bounds,
                    transform,
                    uv,
                    size,
                    ImagePaint {
                        color: batch.brush.image.color.map(|c| c * s.alpha * 0.5 * 0.75),
                        sampling: ImageSampling::with_layout(
                            batch.brush.image.source_rect,
                            layout,
                            [false; 2],
                        ),
                    },
                    at,
                )
                .unwrap();
        }
        assert_eq!(actual.indices, reference.indices);
        assert_eq!(actual.vertices.len(), 8);
        for (a, b) in actual.vertices.iter().zip(&reference.vertices) {
            for (a, b) in a.position.into_iter().zip(b.position) {
                assert!((a - b).abs() < 1e-6);
            }
            assert_eq!(a.uv, b.uv);
            assert_eq!(a.source_window, b.source_window);
            for (a, b) in a.color.into_iter().zip(b.color) {
                assert!((a - b).abs() < 1e-7);
            }
        }
        let clip = RectClip::from_clip(
            &Clip::Fill {
                transform: Affine::IDENTITY,
                shape: Geometry::Rect(Rect::new(80., 40., 130., 75.)),
                fill_rule: Default::default(),
            },
            Affine::IDENTITY,
            size,
        )
        .unwrap();
        let clipped_actual = actual.clipped(clip, allocation()).unwrap().unwrap();
        let clipped_reference = reference.clipped(clip, allocation()).unwrap().unwrap();
        drop((actual, reference));
        let (actual, reference) = (clipped_actual, clipped_reference);
        assert_eq!(actual.indices, reference.indices);
        assert_eq!(actual.vertices.len(), reference.vertices.len());
        for (a, b) in actual.vertices.iter().zip(&reference.vertices) {
            for (a, b) in a.position.into_iter().zip(b.position) {
                assert!((a - b).abs() < 1e-6);
            }
        }
        drop((actual, reference));
        assert_eq!(global.transient.usage().bytes, 0);
    }
    #[test]
    #[ignore = "release CPU sprite expansion and packed vertex-byte measurement"]
    fn sprite_expansion_cost() {
        use std::time::Instant;
        let limits = GpuLimits::default();
        let global = Budgets::new(limits);
        let local = Budgets::new(limits);
        let allocation = || Allocation::new(&global, &local, limits.transient_bytes, 1 << 24);
        let mut batch = batch();
        batch.transform = Affine::IDENTITY;
        batch.bounds = Rect::new(0., 0., 16., 8.);
        batch.brush.image.source_rect = Some(batch.bounds);
        let samples: Vec<_> = (0..4096)
            .map(|i| SpriteInstance {
                position: [16. + (i % 64) as f64 * 15., 16. + (i / 64) as f64 * 15.],
                size: 12.,
                rotation: i as f64 * 0.01,
                alpha: 0.1 + (i % 9) as f32 * 0.1,
            })
            .collect();
        let transforms: Vec<_> = samples
            .iter()
            .map(|s| {
                Affine::translate((s.position[0], s.position[1]))
                    * Affine::rotate(s.rotation)
                    * Affine::scale(s.size / 16.)
                    * Affine::translate((-8., -4.))
            })
            .collect();
        batch.instances = samples.iter().map(|s| s.compiled(batch.bounds)).collect();
        let at = RecordLocation {
            fragment: 0,
            command: 0,
            mask_depth: 0,
            owner: reel_ui::widgets::Label::new("").prepare().id(),
        };
        for compact in [false, true] {
            let mut times = Vec::new();
            let mut bytes = 0;
            let mut vertices = 0;
            for _ in 0..30 {
                let mut mesh = Mesh::new(allocation());
                let started = Instant::now();
                if compact {
                    mesh.sprites(
                        &batch,
                        Affine::IDENTITY,
                        TextureLayout::identity([16, 8]),
                        Affine::scale_non_uniform(1. / 16., 1. / 8.),
                        None,
                        [1024, 1024],
                        at,
                    )
                    .unwrap();
                } else {
                    for (s, transform) in batch.instances.iter().zip(&transforms) {
                        let visible = transform
                            .transform_rect_bbox(batch.bounds)
                            .inflate(1., 1.)
                            .intersect(Rect::new(0., 0., 1024., 1024.));
                        if visible.width() <= 0. || visible.height() <= 0. {
                            continue;
                        }
                        for bounds in crate::common::image_coverage::quads(
                            batch.bounds,
                            &batch.brush,
                            Affine::IDENTITY,
                            *transform,
                            TextureLayout::identity([16, 8]),
                            None,
                        )
                        .iter()
                        {
                            mesh.image_quad(
                                bounds,
                                *transform,
                                Affine::scale_non_uniform(1. / 16., 1. / 8.),
                                [1024, 1024],
                                ImagePaint {
                                    color: batch.brush.image.color.map(|c| {
                                        c * s.alpha
                                            * batch.composite.alpha
                                            * batch.brush.sampler.alpha
                                    }),
                                    sampling: ImageSampling::with_layout(
                                        batch.brush.image.source_rect,
                                        TextureLayout::identity([16, 8]),
                                        [false; 2],
                                    ),
                                },
                                at,
                            )
                            .unwrap();
                        }
                    }
                }
                times.push(started.elapsed().as_micros());
                let upload = crate::common::indices::Upload::new(&mesh, |_| Ok(())).unwrap();
                vertices = mesh.vertices.len();
                bytes = std::mem::size_of_val(mesh.vertices.as_slice()) + upload.bytes().len();
                assert_eq!(mesh.indices.len(), 4096 * 6);
                assert_eq!(vertices, 4096 * 4);
                std::hint::black_box(&mesh);
            }
            times.sort_unstable();
            println!(
                "sprite_mesh compact={compact} median_us={} vertices={vertices} packed_bytes={bytes}",
                times[times.len() / 2]
            );
        }
    }
}
