use super::*;
use reel_ui::{
    core::Widget,
    frame::{FrameGroup, GroupComposite, InkBounds},
    kurbo::Rect,
};
fn group() -> FrameGroup {
    FrameGroup {
        image_mask: None,
        mask_effect: None,
        clip: None,
        mask: None,
        filters: vec![],
        composite: GroupComposite {
            alpha: 0.5,
            blend: Default::default(),
        },
        invert: 0.,
        owner: reel_ui::widgets::Label::new("").prepare().id(),
        effect_to_target: Affine::IDENTITY,
        ink_bounds: InkBounds::Unbounded,
        effect: Some(FrameEffect {
            definition: [0; 32],
            shader: reel_ui::resources::ResourceHandle {
                registry: 1,
                slot: 0,
                generation: 1,
            },
            values: vec![],
            images: vec![],
            bounds: Rect::new(0., 0., 160., 120.),
            coverage_invert: false,
            resolution_scale: 1.,
        }),
    }
}
#[test]
fn exact_effect_grid_keeps_shared_atlas_regions_owned_by_the_batch() {
    // A clipped 52x48 region can contain a sole effect whose complete domain
    // matches the 64x224 atlas. Grid equality alone must not transfer its owner.
    let mut child = group();
    child.composite.alpha = 1.;
    child.effect.as_mut().unwrap().bounds = Rect::new(0., 0., 64., 224.);
    child.effect_to_target = Affine::translate((-1., -51.));
    let parent_transform = Affine::translate((1., 51.));
    let source = source_layout(
        child.effect.as_ref().unwrap(),
        child.effect_to_target,
        parent_transform,
        4096,
        RecordLocation {
            fragment: 0,
            command: 0,
            mask_depth: 0,
            owner: child.owner,
        },
    )
    .unwrap();
    assert_eq!(source.size, [64, 224]);
    assert_eq!(source.frame_to_source, parent_transform);
    assert!(forwards_source(
        &child,
        &source,
        parent_transform,
        [64, 224],
        false
    ));
    assert!(!forwards_source(
        &child,
        &source,
        parent_transform,
        [64, 224],
        true
    ));
}
#[test]
fn terminal_effect_proof_keeps_sampling_boundaries_and_precision_sensitive_operations() {
    let mut group = group();
    assert!(terminal_composite(&group, 1, false));
    assert!(!terminal_composite(&group, 4, false));
    assert!(!terminal_composite(&group, 1, true));
    group.invert = 0.01;
    assert!(!terminal_composite(&group, 1, false));
    group.invert = 0.;
    group.composite.blend = reel_ui::peniko::Mix::Screen.into();
    assert!(!terminal_composite(&group, 1, false));
    group.composite.blend = Default::default();
    group.mask_effect = group.effect.clone();
    assert!(!terminal_composite(&group, 1, false));
    group.mask_effect = None;
    group.mask = Some(reel_ui::frame::MaskUse {
        mask: 0,
        transform: Affine::IDENTITY,
    });
    assert!(!terminal_composite(&group, 1, false));
}
#[test]
fn terminal_unorm_ties_bound_translucent_source_over_error_after_opacity() {
    // Adversarial nearest-store tie choices: RGB rounds up while alpha rounds
    // down. The alternative rounds both the opposite way. This covers the
    // largest error permitted by unspecified hardware halfway tie direction.
    let mut maximum = [0f64; 2];
    for color in 0..255 {
        for alpha in 0..255 {
            for opacity in [0., 0.01, 0.37, 0.75, 1.] {
                for background_alpha in [0., 0.25, 0.5, 0.9, 1.] {
                    let background_color = background_alpha * 0.8;
                    let a = alpha as f64 / 255. * opacity;
                    let b = (alpha + 1) as f64 / 255. * opacity;
                    let fused = (color + 1) as f64 / 255. * opacity + background_color * (1. - a);
                    let stored = color as f64 / 255. * opacity + background_color * (1. - b);
                    let rgb = (fused - stored).abs();
                    let alpha = ((a + background_alpha * (1. - a))
                        - (b + background_alpha * (1. - b)))
                        .abs();
                    maximum[0] = maximum[0].max(rgb);
                    maximum[1] = maximum[1].max(alpha);
                    assert!(rgb <= 2. / 255. + 1e-14);
                    assert!(alpha <= 1. / 255. + 1e-14);
                }
            }
        }
    }
    eprintln!("terminal UNORM opposite-tie source-over max RGB/alpha: {maximum:?}");
}
#[test]
fn terminal_half_rounding_preserves_every_finite_attachment_value() {
    for bits in 0..=u16::MAX {
        let value = half::f16::from_bits(bits);
        if value.is_finite() {
            assert_eq!(half::f16::from_f32(value.to_f32()).to_bits(), bits);
        }
    }
}

#[test]
fn nested_clip_keeps_both_stores_before_independent_coverage() {
    let up = |v: f64| (v.clamp(0., 1.) * 255. + 0.5).floor() / 255.;
    let down = |v: f64| (v.clamp(0., 1.) * 255. - 0.5).ceil() / 255.;
    let mut maximum = [0f64; 2];
    for color in 0..255 {
        for alpha in 0..255 {
            for opacity in [0., 0.01, 0.37, 0.5, 0.75, 1.] {
                let color_a = up((color + 1) as f64 / 255. * opacity);
                let color_b = down(color as f64 / 255. * opacity);
                let alpha_a = down(alpha as f64 / 255. * opacity);
                let alpha_b = up((alpha + 1) as f64 / 255. * opacity);
                for coverage in [0., 0.1, 0.5, 0.9, 1.] {
                    for background in [0., 0.25, 0.5, 0.9, 1.] {
                        let a = alpha_a * coverage;
                        let b = alpha_b * coverage;
                        let rgb =
                            ((color_a - color_b) * coverage + background * 0.8 * (b - a)).abs();
                        let alpha =
                            ((a + background * (1. - a)) - (b + background * (1. - b))).abs();
                        maximum[0] = maximum[0].max(rgb);
                        maximum[1] = maximum[1].max(alpha);
                        assert!(rgb <= 4. / 255. + 1e-14);
                        assert!(alpha <= 2. / 255. + 1e-14);
                    }
                }
            }
        }
    }
    // Collapsing the two stores changes even an ordinary half-opacity sample.
    let input = 0.5 / 255.;
    let opacity = 0.5;
    let coverage = 0.5;
    assert_ne!(
        up(up(input) * opacity) * coverage,
        up(input) * opacity * coverage
    );
    eprintln!("two-store coverage source-over max RGB/alpha: {maximum:?}");
    for bits in 0..=u16::MAX {
        let input = half::f16::from_bits(bits);
        if input.is_finite() {
            for opacity in [0., 0.37, 1.] {
                let stored = half::f16::from_f32(input.to_f32() * opacity).to_f32();
                for coverage in [0., 0.1, 0.5, 1.] {
                    let original = stored * coverage;
                    let fused =
                        half::f16::from_f32(half::f16::from_f32(input.to_f32()).to_f32() * opacity)
                            .to_f32()
                            * coverage;
                    assert_eq!(original.to_bits(), fused.to_bits());
                }
            }
        }
    }
}

#[test]
fn coverage_tail_accounts_for_replaced_half_composite_products() {
    let half = |v: f32| half::f16::from_f32(v).to_f32();
    let mut max = 0f32;
    for stored in 0..=255 {
        for coverage in 0..=255 {
            let stored = stored as f32 / 255.;
            let coverage = coverage as f32 / 255.;
            for opacity in [0., 0.01, 0.37, 0.75, 1.] {
                let original = half(half(half(stored) * half(coverage)) * half(opacity));
                let fused = stored * coverage * opacity;
                max = max.max((fused - original).abs());
                assert!((fused - original).abs() <= 5. / 4096.);
            }
        }
    }
    eprintln!(
        "replaced half coverage multiply max channel error={max}; source-over bound RGB=10/4096, alpha=5/4096"
    );
}
