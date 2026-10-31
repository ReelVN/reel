use super::*;
use crate::common::mesh::DirectPaint;

#[test]
fn monochrome_requires_equal_finite_channels_and_proves_opaque_white_separately() {
    let mut values = [[0.; 4]; 10];
    values[4][3] = 1.;
    values[6] = [1.1, 1.1, 1.1, 0.];
    values[7] = [-0.04, -0.04, -0.04, 1.];
    let classify =
        |kind, values: &_, color| monochrome_adjustment(Some(kind), values, [color].into_iter());
    for kind in [DirectPaint::Bc1, DirectPaint::RegionalBc1] {
        for alpha in [0., f32::MIN_POSITIVE, 0.37, 1.] {
            assert_eq!(
                classify(kind, &values, [alpha; 4]),
                Some(Monochrome::OpaqueWhite)
            );
        }
        assert_eq!(
            classify(kind, &values, [0.1, 0.2, 0.3, 0.5]),
            Some(Monochrome::Premultiplied)
        );
    }
    for kind in [DirectPaint::Bc3, DirectPaint::RegionalBc3] {
        assert_eq!(
            classify(kind, &values, [1.; 4]),
            Some(Monochrome::Premultiplied)
        );
    }
    for kind in [
        DirectPaint::Rgba,
        DirectPaint::RegionalRgba,
        DirectPaint::Solid,
        DirectPaint::Coverage,
    ] {
        assert_eq!(classify(kind, &values, [1.; 4]), None);
    }
    for (row, column, value) in [
        (4, 3, 3.),
        (5, 3, 1.),
        (6, 1, 0.7),
        (6, 3, 0.1),
        (7, 1, 0.2),
        (7, 3, 0.),
        (6, 0, f32::NAN),
        (7, 0, f32::INFINITY),
    ] {
        let mut changed = values;
        changed[row][column] = value;
        assert_eq!(classify(DirectPaint::Bc1, &changed, [1.; 4]), None);
    }
    for color in [
        [1., 1., 1., 0.],
        [-0.1, 0., 0., 1.],
        [0., 0., 0., 1.1],
        [f32::NAN, 0., 0., 1.],
    ] {
        assert_eq!(classify(DirectPaint::Bc1, &values, color), None);
    }
    values[6] = [3., 3., 3., 0.];
    assert_eq!(
        classify(DirectPaint::Bc1, &values, [1.; 4]),
        Some(Monochrome::Premultiplied)
    );
}

#[test]
fn moving_white_opacity_after_monochrome_preserves_bounded_f32_color() {
    let encode = |x: f32| {
        if x <= 0.0031308 {
            x * 12.92
        } else {
            1.055 * x.powf(1. / 2.4) - 0.055
        }
    };
    let decode = |x: f32| {
        if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };
    let gray = |rgb: [f32; 3]| rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
    let mut maximum = 0f32;
    for sample in 0..=1024 {
        let source = [
            sample as f32 / 1024.,
            ((sample * 431) % 1025) as f32 / 1024.,
            ((sample * 769) % 1025) as f32 / 1024.,
        ];
        for opacity in [0., 1e-40, 1e-20, 1e-8, 1e-4, 0.03, 0.37, 0.9, 1.] {
            let before = if opacity > 0. {
                gray(source.map(|x| encode((x * opacity) / opacity)))
            } else {
                0.
            };
            let after = gray(source.map(encode));
            for gain in [-2., -0.3, 0., 0.37, 1., 1.0851064, 2.] {
                for offset in [-1., -0.039215688, 0., 0.3, 1.] {
                    let expected = decode((before * gain + offset).clamp(0., 1.)) * opacity;
                    let actual = decode((after * gain + offset).clamp(0., 1.)) * opacity;
                    maximum = maximum.max((actual - expected).abs());
                }
            }
        }
    }
    assert!(
        maximum < 1. / 65536.,
        "opacity cancellation error: {maximum}"
    );
}

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
