use super::*;
use crate::common::{GpuLimits, mesh::Allocation, renderer::Budgets};
use reel_ui::kurbo::Rect;

fn quad(uv: Rect) -> Mesh {
    let limits = GpuLimits::default();
    let shared = Budgets::new(limits);
    let local = Budgets::new(limits);
    let mut mesh = Mesh::new(Allocation::new(
        &shared,
        &local,
        limits.transient_bytes,
        1 << 20,
    ));
    mesh.quad(Rect::new(-1., -1., 1., 1.), uv).unwrap();
    for vertex in &mut mesh.vertices {
        vertex.source_window = [0.125, 0.125, 0.75, 0.75];
    }
    mesh
}

#[test]
fn atlas_edges_keep_the_regional_program_and_most_pixels_use_the_same_direct_sample() {
    let viewport = [960, 544];
    let outer = Scissor::full(viewport);
    for (u0, u1) in [(0.125, 0.875), (0.875, 0.125)] {
        for (v0, v1) in [(0.125, 0.875), (0.875, 0.125)] {
            let mesh = quad(Rect::new(u0, v0, u1, v1));
            assert_eq!(
                direct_paint(
                    &mesh,
                    Some(DirectPaint::RegionalBc3),
                    [1024; 2],
                    viewport,
                    outer,
                    1
                ),
                Some(DirectPaint::RegionalBc3)
            );
            let inner = regional_interior(&mesh, [1024; 2], viewport, outer, 1).unwrap();
            assert_eq!(
                inner,
                Scissor {
                    x: 32,
                    y: 32,
                    width: 896,
                    height: 480
                }
            );
            for direct in [
                DirectPaint::RegionalRgba,
                DirectPaint::RegionalBc1,
                DirectPaint::RegionalBc3,
            ] {
                assert_ne!(
                    direct_paint(&mesh, Some(direct), [1024; 2], viewport, inner, 1),
                    Some(direct)
                );
            }
            for y in (inner.y..inner.y + inner.height).step_by(7) {
                for x in (inner.x..inner.x + inner.width).step_by(7) {
                    for offset in [-0.9375, 0.5, 1.9375] {
                        let uv = [
                            (u0 + (u1 - u0) * (f64::from(x) + offset) / 960.) as f32,
                            (v1 + (v0 - v1) * (f64::from(y) + offset) / 544.) as f32,
                        ];
                        for value in uv {
                            assert_eq!(
                                value.to_bits(),
                                value
                                    .clamp(0.125 + 0.5 / 1024., 0.875 - 0.5 / 1024.)
                                    .to_bits()
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn scissor_partition_has_exactly_one_owner_for_every_original_pixel() {
    for outer in [
        Scissor::full([960, 544]),
        Scissor {
            x: 77,
            y: 29,
            width: 700,
            height: 460,
        },
    ] {
        let mesh = quad(Rect::new(0.125, 0.125, 0.875, 0.875));
        let inner = regional_interior(&mesh, [1024; 2], [960, 544], outer, 1).unwrap();
        let parts = partition(outer, inner);
        assert!(inner.tile_exact(32, [960, 544]));
        assert_eq!(
            parts
                .iter()
                .map(|r| u64::from(r.width) * u64::from(r.height))
                .sum::<u64>(),
            u64::from(outer.width) * u64::from(outer.height)
        );
        for y in 0..544 {
            for x in 0..960 {
                let contains =
                    |r: &Scissor| x >= r.x && y >= r.y && x < r.x + r.width && y < r.y + r.height;
                assert_eq!(
                    parts.iter().filter(|r| contains(r)).count(),
                    usize::from(contains(&outer))
                );
            }
        }
    }
}

#[test]
fn device_pixel_fixture_uses_the_production_interior() {
    let mut mesh = quad(Rect::new(0.125, 0.25, 0.875, 0.75));
    for vertex in &mut mesh.vertices {
        vertex.source_window = [0.125, 0.25, 0.75, 0.5];
    }
    assert_eq!(
        regional_interior(&mesh, [64, 32], [640, 480], Scissor::full([640, 480]), 1),
        Some(Scissor {
            x: 32,
            y: 32,
            width: 576,
            height: 416
        })
    );
}

#[test]
fn mipmaps_small_interiors_shear_and_discontinuous_uvs_keep_one_original_draw() {
    let mut mesh = quad(Rect::new(0.125, 0.125, 0.875, 0.875));
    let viewport = [960, 544];
    let outer = Scissor::full(viewport);
    assert!(regional_interior(&mesh, [1024; 2], viewport, outer, 2).is_none());
    assert!(regional_interior(&mesh, [1024; 2], [128; 2], Scissor::full([128; 2]), 1).is_none());
    mesh.vertices[0].uv[0] = 0.2;
    assert!(regional_interior(&mesh, [1024; 2], viewport, outer, 1).is_none());
    mesh.vertices[0].uv[0] = 0.125;
    mesh.vertices[0].position[0] += 0.1;
    assert!(regional_interior(&mesh, [1024; 2], viewport, outer, 1).is_none());
    mesh.vertices[0].position[0] = f32::NAN;
    assert!(regional_interior(&mesh, [1024; 2], viewport, outer, 1).is_none());
}

#[test]
fn coverage_bands_keep_their_original_mesh_and_admit_a_shared_interior() {
    use crate::common::{
        image_coverage,
        mesh::{ImagePaint, ImageSampling},
    };
    use reel_core::{image_coverage::ImageCoverage, texture::TextureLayout};
    use reel_ui::{
        frame::{FrameImage, RecordLocation},
        kurbo::Affine,
        peniko::{ImageBrush, ImageSampler},
        resources::ResourceHandle,
    };

    let mut pixels = vec![0; 64 * 64 * 4];
    for y in 0..64 {
        let inset = if y < 20 {
            3
        } else if y < 40 {
            0
        } else {
            6
        };
        for x in inset..64 - inset {
            pixels[(y * 64 + x) * 4 + 3] = 128;
        }
    }
    let coverage = ImageCoverage::rgba8([64, 64], &pixels).unwrap();
    let layout = TextureLayout {
        logical_size: [1024, 576],
        content_size: [64, 64],
        storage_size: [128, 128],
    };
    let brush = ImageBrush {
        image: FrameImage {
            resource: ResourceHandle {
                registry: 1,
                slot: 0,
                generation: 1,
            },
            color: [1.; 4],
            appearance: None,
            source_rect: None,
        },
        sampler: ImageSampler::default(),
    };
    let viewport = [960, 544];
    let transform = Affine::scale_non_uniform(960. / 1024., 544. / 576.);
    let mut mesh = quad(Rect::ZERO);
    mesh.vertices.clear();
    mesh.indices.clear();
    for bounds in image_coverage::quads(
        Rect::new(0., 0., 1024., 576.),
        &brush,
        Affine::IDENTITY,
        transform,
        layout,
        Some(&coverage),
    )
    .iter()
    {
        mesh.image_quad(
            bounds,
            transform,
            Affine::scale_non_uniform(layout.uv_scale()[0], layout.uv_scale()[1]),
            viewport,
            ImagePaint {
                color: [1.; 4],
                sampling: ImageSampling::for_image(None, layout, [0.; 2], [false; 2], false),
            },
            RecordLocation {
                fragment: 0,
                command: 0,
                mask_depth: 0,
                owner: reel_ui::core::Widget::prepare(reel_ui::widgets::Label::new("")).id(),
            },
        )
        .unwrap();
    }
    assert!(
        mesh.vertices.len() > 4,
        "the production coverage builder must split this image"
    );
    let original_vertices = mesh.vertices.len();
    let original_indices = mesh.indices.len();
    let inner = regional_interior(
        &mesh,
        layout.storage_size,
        viewport,
        Scissor::full(viewport),
        1,
    )
    .unwrap();
    assert_eq!(
        direct_paint(
            &mesh,
            Some(DirectPaint::RegionalBc3),
            layout.storage_size,
            viewport,
            inner,
            1
        ),
        Some(DirectPaint::Bc3)
    );
    assert_eq!(
        (mesh.vertices.len(), mesh.indices.len()),
        (original_vertices, original_indices)
    );
    // A second independent image's discontinuous map must not share that clip.
    mesh.vertices.last_mut().unwrap().uv[0] += 0.1;
    assert!(
        regional_interior(
            &mesh,
            layout.storage_size,
            viewport,
            Scissor::full(viewport),
            1
        )
        .is_none()
    );
}
