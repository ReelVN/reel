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
        }),
    }
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
