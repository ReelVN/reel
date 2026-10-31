use super::*;
use crate::common::mesh::DirectPaint;

#[test]
fn alpha_tint_requires_block_alpha_and_premultiplied_finite_vertex_colors() {
    let mut appearance = [[0.; 4]; 10];
    appearance[4][3] = 1.;
    appearance[7] = [0.2, 0.8, 1., 1.];
    let admits =
        |kind, values: &_, color| constant_alpha_tint(Some(kind), values, [color].into_iter());
    for kind in [
        DirectPaint::Bc1,
        DirectPaint::Bc3,
        DirectPaint::RegionalBc1,
        DirectPaint::RegionalBc3,
    ] {
        for color in [[0.; 4], [0.1, 0.2, 0.4, 0.5], [1.; 4]] {
            assert!(admits(kind, &appearance, color));
        }
        for color in [
            [1., 0., 0., 0.],
            [-0.1, 0., 0., 1.],
            [0., 0., 0., 1.01],
            [0., 0., 0., f32::NAN],
            [f32::NAN, 0., 0., 1.],
        ] {
            assert!(!admits(kind, &appearance, color));
        }
    }
    for kind in [
        DirectPaint::Rgba,
        DirectPaint::RegionalRgba,
        DirectPaint::Coverage,
        DirectPaint::Solid,
    ] {
        assert!(!admits(kind, &appearance, [1.; 4]));
    }
    for (row, column, value) in [
        (4, 3, 3.),
        (5, 3, 1.),
        (6, 0, 0.1),
        (7, 3, 0.),
        (7, 0, f32::NAN),
    ] {
        let mut changed = appearance;
        changed[row][column] = value;
        assert!(!admits(DirectPaint::Bc3, &changed, [1.; 4]));
    }
}

#[test]
fn alpha_only_tint_matches_zero_gain_and_stays_in_the_bounded_color_error_domain() {
    let decode = |v: f64| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let h = |v: f32| {
        let v = half::f16::from_f32(v).to_f32();
        if v.abs() < 2f32.powi(-14) { 0. } else { v }
    };
    let mut maximum = 0f64;
    let opacities = [0., 1e-40, 1e-8, 0.1, 0.51, 0.9, 1.];
    for source_alpha in 0..=255 {
        let source_alpha = f64::from(source_alpha) / 255.;
        for opacity in opacities {
            for offset in [-0.2_f64, 0., 0.03, 0.37, 0.62, 0.9, 1.2] {
                let expected = decode(offset.clamp(0., 1.)) * source_alpha * opacity;
                let actual = h(
                    h(h(decode(offset.clamp(0., 1.)) as f32) * h(source_alpha as f32))
                        * h(opacity as f32),
                );
                maximum = maximum.max((f64::from(actual) - expected).abs());
            }
        }
    }
    assert!(maximum <= 4. / 4096., "bounded alpha tint error: {maximum}");
    let mut blend_error = 0f64;
    for a in opacities {
        for b in opacities {
            for step in 0..=16 {
                let t = f64::from(step) / 16.;
                let opacity = a * (1. - t) + b * t;
                let vertex_alpha = h(h(a as f32) * (1. - t as f32) + h(b as f32) * t as f32);
                for source in 0..=255 {
                    let source = f64::from(source) / 255.;
                    let alpha = h(h(source as f32) * vertex_alpha);
                    for offset in [0., 0.03, 0.37, 0.9, 1.] {
                        let rgb = h(h(h(decode(offset) as f32) * h(source as f32)) * vertex_alpha);
                        for background in [0., 0.37, 1.] {
                            let expected = decode(offset) * source * opacity
                                + background * (1. - source * opacity);
                            let actual = f64::from(rgb) + background * (1. - f64::from(alpha));
                            blend_error = blend_error.max((actual - expected).abs());
                        }
                    }
                }
            }
        }
    }
    // Uniform color, filtered alpha, vertex interpolation and two products
    // contribute six roundings to RGB and four to alpha under source-over.
    // The resulting 10/4096 bound is below one UNORM byte (1/255).
    assert!(
        blend_error <= 10. / 4096.,
        "bounded tint blend error: {blend_error}"
    );
}
