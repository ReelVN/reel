use super::*;
use crate::common::{GpuLimits, mesh::Allocation, renderer::Budgets};
use reel_ui::kurbo::{Affine, Point, Rect};

fn geometry() -> Mesh {
    let limits = GpuLimits::default();
    let shared = Budgets::new(limits);
    let local = Budgets::new(limits);
    let mut mesh = Mesh::new(Allocation::new(
        &shared,
        &local,
        limits.transient_bytes,
        1 << 20,
    ));
    mesh.quad(Rect::new(-0.7, -0.8, 0.7, 0.8), Rect::new(0.1, 0., 0.6, 1.))
        .unwrap();
    for vertex in &mut mesh.vertices {
        vertex.source_window = [0., 0., 0.75, 1.];
    }
    mesh
}

#[test]
fn padded_interior_keeps_block_alpha_kinds_and_rejects_active_edges_and_mip_lod_changes() {
    let mut mesh = geometry();
    for (regional, direct) in [
        (DirectPaint::RegionalRgba, DirectPaint::Rgba),
        (DirectPaint::RegionalBc1, DirectPaint::Bc1),
        (DirectPaint::RegionalBc3, DirectPaint::Bc3),
    ] {
        assert_eq!(
            direct_paint(&mesh, Some(regional), [512; 2], [960, 544], 1),
            Some(direct)
        );
        assert_eq!(
            direct_paint(&mesh, Some(regional), [512; 2], [960, 544], 2),
            Some(regional)
        );
    }
    assert_eq!(direct_paint(&mesh, None, [512; 2], [960, 544], 1), None);
    let regional = Some(DirectPaint::RegionalBc3);
    for edge in [0., 0.75, 1.] {
        mesh.vertices[0].uv[0] = edge;
        assert_eq!(
            direct_paint(&mesh, regional, [512; 2], [960, 544], 1),
            regional
        );
    }
    mesh.vertices[0].uv[0] = 0.1;
    assert_eq!(direct_paint(&mesh, regional, [512; 2], [1, 1], 1), regional);
    mesh.vertices[0].source_window[0] = 0.01;
    assert_eq!(
        direct_paint(&mesh, regional, [512; 2], [960, 544], 1),
        regional
    );
}

fn sample_uv(at: Point, window: [f32; 4], regional: bool) -> [f64; 2] {
    let mut uv = [at.x, at.y];
    for axis in 0..2 {
        let half: f64 = 0.5 / 512.;
        if regional {
            let low = f64::from(window[axis]);
            let high = low + f64::from(window[axis + 2]);
            let inset = half.min((high - low) * 0.5);
            uv[axis] = uv[axis].clamp(low + inset, high - inset);
        }
        uv[axis] = uv[axis].clamp(half, 1. - half);
    }
    uv
}

#[test]
fn admitted_rotated_mirrored_and_sheared_triangles_match_regional_sampling_at_raster_offsets() {
    let local = [Point::new(0., 0.), Point::new(1., 0.), Point::new(0., 1.)];
    let uv = Affine::translate((0.1, 0.2)) * Affine::scale_non_uniform(0.5, 0.4);
    let window = [0., 0.1, 0.75, 0.7];
    for scale in [16., 64., 512.] {
        for mirror in [-1., 1.] {
            let physical = Affine::translate((400., 200.))
                * Affine::rotate(0.71)
                * Affine::new([scale * mirror, 0., scale * 0.2, scale, 0., 0.]);
            let vertices = local.map(|p| {
                let position = physical * p;
                let uv = uv * p;
                Vertex {
                    position: [
                        (position.x / 480. - 1.) as f32,
                        (1. - position.y / 272.) as f32,
                    ],
                    uv: [uv.x as f32, uv.y as f32],
                    color: [1.; 4],
                    source_window: window,
                }
            });
            assert!(triangle(vertices.each_ref(), [512; 2], [960, 544]));
            for y in 0..=32 {
                for x in 0..=32 - y {
                    let p = Point::new(x as f64 / 32., y as f64 / 32.);
                    for offset in [
                        (-0.9375, -0.9375),
                        (0.9375, -0.9375),
                        (-0.9375, 0.9375),
                        (0.9375, 0.9375),
                    ] {
                        let sample = uv
                            * (physical.inverse()
                                * (physical * p + reel_ui::kurbo::Vec2::from(offset)));
                        assert_eq!(
                            sample_uv(sample, window, false),
                            sample_uv(sample, window, true)
                        );
                    }
                }
            }
        }
    }
}
