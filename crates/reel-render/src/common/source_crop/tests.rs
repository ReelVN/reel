use super::*;

#[test]
fn crop_backing_restores_the_original_cache_content_grid() {
    use crate::common::effect_result::Grid;
    let crop = Crop {
        logical: [2048, 512],
        physical: None,
        origin: [1024, 0],
        size: [1024, 512],
    };
    let original = Affine::new([1., 0., 0., 1., 0.0625, 0.125]);
    let region = Scissor {
        x: 1200,
        y: 100,
        width: 640,
        height: 300,
    };
    let (source_transform, effect_transform) = Grid::transforms(original, Affine::IDENTITY);
    let original = Grid {
        source_transform,
        effect_transform,
        effect_context: None,
        source_size: crop.logical,
        source_storage: crop.logical,
        source_scissor: [region.x, region.y, region.width, region.height],
        output_size: [640, 256],
        output_storage: [640, 256],
        scissor: [0, 0, 640, 256],
        quality: [0, 1],
    };
    let mut physical = original;
    physical.source_transform = crop
        .transform(Affine::new(original.source_transform.map(f64::from_bits)))
        .as_coeffs()
        .map(f64::to_bits);
    physical.source_size = crop.size;
    physical.source_storage = crop.size;
    let local = crop.scissor(region);
    physical.source_scissor = [local.x, local.y, local.width, local.height];
    assert!(crop.original_grid(physical) == original);
}

#[test]
fn non_power_of_two_edge_clamps_keep_the_original_axis() {
    let crop = Crop::window(
        [1920, 400],
        Scissor {
            x: 4,
            y: 320,
            width: 80,
            height: 80,
        },
    )
    .unwrap();
    assert_eq!(crop.size[1], 400);
    assert!(crop.size[0] < 1920);
    let crop = Crop::window(
        [2048, 512],
        Scissor {
            x: 0,
            y: 0,
            width: 80,
            height: 36,
        },
    )
    .unwrap();
    assert_eq!(crop.size, [128, 64]);
}

#[test]
fn zero_origin_lower_clamp_has_identical_texel_phase() {
    let crop = Crop::window(
        [1920, 400],
        Scissor {
            x: 0,
            y: 0,
            width: 868,
            height: 274,
        },
    )
    .unwrap();
    assert_eq!(crop.size, [960, 400]);
    assert_eq!(crop.origin, [0, 0]);
    let scale = 2.;
    let old_half = 0.5f32 / 1920.;
    let new_half = 0.5f32 / 960.;
    assert_eq!(old_half * scale, new_half);
    for uv in [
        -1.,
        -0.,
        0.,
        old_half / 2.,
        old_half,
        old_half * 2.,
        0.25,
        0.45,
    ] {
        let original = uv.clamp(0., 1.).clamp(old_half, 1. - old_half);
        let cropped = (uv.clamp(0., 1.) * scale).clamp(new_half, scale - new_half);
        assert_eq!(f64::from(original) * 1920., f64::from(cropped) * 960.);
    }
}

#[test]
fn dyadic_windows_keep_the_entire_filter_demand_and_boundaries() {
    for logical in [[1920, 400], [2048, 512], [1024, 576]] {
        for demand in [
            Scissor {
                x: 4,
                y: 4,
                width: 80,
                height: 36,
            },
            Scissor {
                x: logical[0] - 100,
                y: logical[1] - 60,
                width: 80,
                height: 36,
            },
        ] {
            let crop = Crop::window(logical, demand).unwrap();
            assert!(crop.size[0] * crop.size[1] < logical[0] * logical[1]);
            let local = crop.scissor(demand);
            assert!(local.x + local.width <= crop.size[0]);
            assert!(local.y + local.height <= crop.size[1]);
            for (axis, logical) in logical.into_iter().enumerate() {
                assert!((logical / crop.size[axis]).is_power_of_two());
                assert_eq!(logical % crop.size[axis], 0);
                assert_eq!(crop.origin[axis] % crop.size[axis], 0);
            }
        }
    }
    assert!(Crop::window([1920, 400], Scissor::full([1920, 400])).is_none());
    assert!(Crop::window([1920, 400], Scissor::full([1921, 400])).is_none());
}

#[test]
fn source_domain_and_opacity_survive_the_smaller_texture_mapping() {
    let crop = Crop {
        logical: [1920, 400],
        physical: None,
        origin: [960, 200],
        size: [480, 100],
    };
    let mut context = [[0.; 4]; crate::effect::CONTEXT_SLOTS];
    context[2][3] = 0.375;
    context[4] = [13., 7., 2048., 512.];
    crop.map(&mut context);
    assert_eq!(context[4], [13., 7., 2048., 512.]);
    assert_eq!(context[2], [4., 0., -2., 0.375]);
    assert_eq!(context[3], [0., 4., -2., 0.]);
    assert_eq!(context[5][2..], [480., 100.]);
    assert_eq!(context[6], [-2., -2., 4., 4.]);
    // The original source's edge clamp is not the crop's [0,1] interval.
    assert!(context[6][0] + 0.5 / context[5][2] < 0.);
    assert!(context[6][0] + context[6][2] - 0.5 / context[5][2] > 1.);
}

#[test]
fn dyadic_affine_preserves_exact_real_texel_phase_for_every_f32_inside_the_window() {
    let crop = Crop {
        logical: [1920, 400],
        physical: None,
        origin: [960, 200],
        size: [480, 100],
    };
    for axis in 0..2 {
        let scale = (crop.logical[axis] / crop.size[axis]) as f32;
        let cell = (crop.origin[axis] / crop.size[axis]) as f32;
        for index in 0..=8192 {
            let uv = (crop.origin[axis] as f32 + (index as f32 / 8192.) * crop.size[axis] as f32)
                / crop.logical[axis] as f32;
            let q = uv.mul_add(scale, -cell);
            assert_eq!(
                q,
                uv * scale - cell,
                "power-of-two product and exact subtraction"
            );
            assert_eq!(
                f64::from(q) * f64::from(crop.size[axis]) + f64::from(crop.origin[axis]),
                f64::from(uv) * f64::from(crop.logical[axis])
            );
        }
    }
}
