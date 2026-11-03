use super::*;
use reel_ui::{
    core::Widget,
    frame::{FrameGroup, GroupComposite, InkBounds},
    kurbo::Rect,
};

fn group(transform: Affine) -> FrameGroup {
    FrameGroup {
        image_mask: None,
        mask_effect: None,
        clip: None,
        mask: None,
        filters: vec![],
        composite: GroupComposite {
            alpha: 1.,
            blend: Default::default(),
        },
        invert: 0.,
        owner: reel_ui::widgets::Label::new("").prepare().id(),
        effect_to_target: transform,
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
            bounds: Rect::new(-3.25, 4.75, 140.25, 91.5),
            coverage_invert: false,
            resolution_scale: 1.,
        }),
    }
}
fn layout(group: &FrameGroup) -> SourceLayout {
    source_layout(
        group.effect.as_ref().unwrap(),
        group.effect_to_target,
        Affine::IDENTITY,
        4096,
        RecordLocation {
            fragment: 0,
            command: 0,
            mask_depth: 0,
            owner: group.owner,
        },
    )
    .unwrap()
}

#[test]
fn shared_local_grid_keeps_frame_geometry_and_removes_world_cancellation() {
    let mut reference = None;
    let mut changed = false;
    for (x, y, rotation) in [
        (0., 0., 0.),
        (0.125, -0.375, 0.),
        (17.3, 81.7, 0.37),
        (-1003.17, 451.83, -0.82),
    ] {
        let parent = group(
            Affine::translate((x, y))
                * Affine::rotate(rotation)
                * Affine::scale_non_uniform(0.837, 1.213),
        );
        let source = layout(&parent);
        let effect = parent.effect.as_ref().unwrap();
        let bounds = effect.bounds;
        let expected = Affine::scale_non_uniform(
            f64::from(source.size[0]) / bounds.width(),
            f64::from(source.size[1]) / bounds.height(),
        ) * Affine::translate((-bounds.x0, -bounds.y0));
        assert_eq!(source.effect_to_source, expected);
        // Geometry keeps the original inverse and multiplication order.
        assert_eq!(
            source.frame_to_source,
            expected * parent.effect_to_target.inverse()
        );
        let local = output_transform(
            &parent,
            Some((&parent, source.effect_to_source)),
            source.frame_to_source,
        );
        assert_eq!(local, expected);
        let legacy = source.frame_to_source * parent.effect_to_target;
        changed |= legacy != local;
        let invocation = context(effect, local, source.size, source.size).unwrap();
        let bits = invocation.map(|row| row.map(f32::to_bits));
        if let Some((size, before)) = &reference {
            assert_eq!(*size, source.size);
            assert_eq!(*before, bits);
        } else {
            reference = Some((source.size, bits));
        }
    }
    assert!(
        changed,
        "the fixture must exercise floating point cancellation"
    );
}

#[test]
fn local_grid_requires_the_same_effect_domain_and_an_established_parent_raster() {
    let parent = group(Affine::translate((123.4, 51.6)) * Affine::rotate(0.23));
    let source = layout(&parent);
    let legacy = source.frame_to_source * parent.effect_to_target;
    assert_eq!(
        output_transform(&parent, None, source.frame_to_source),
        legacy
    );
    let mut child = parent.clone();
    child.effect.as_mut().unwrap().bounds.x1 -= 0.25;
    assert_eq!(
        output_transform(
            &child,
            Some((&parent, source.effect_to_source)),
            source.frame_to_source
        ),
        source.frame_to_source * child.effect_to_target,
    );
    child = parent.clone();
    child.effect_to_target *= Affine::translate((0.125, 0.));
    assert_eq!(
        output_transform(
            &child,
            Some((&parent, source.effect_to_source)),
            source.frame_to_source
        ),
        source.frame_to_source * child.effect_to_target,
    );
    child.effect = None;
    assert_eq!(
        output_transform(
            &child,
            Some((&parent, source.effect_to_source)),
            source.frame_to_source
        ),
        source.frame_to_source * child.effect_to_target,
    );
}

#[test]
fn local_grid_preserves_crop_placement_and_degenerate_domains() {
    let parent = group(Affine::translate((53.7, -27.3)) * Affine::rotate(0.67));
    let source = layout(&parent);
    let crop = crate::common::source_crop::Crop {
        logical: source.size,
        origin: [32, 16],
        size: [64, 64],
        physical: None,
    };
    let cropped = crop.transform(source.effect_to_source);
    assert_eq!(
        output_transform(
            &parent,
            Some((&parent, cropped)),
            crop.transform(source.frame_to_source)
        ),
        Affine::translate((-32., -16.)) * source.effect_to_source,
    );
    let mut degenerate = parent.clone();
    degenerate.effect_to_target = Affine::scale(0.);
    let source = layout(&degenerate);
    assert_eq!(source.effect_to_source, Affine::scale(0.));
    assert_eq!(source.frame_to_source, Affine::scale(0.));
    assert!(
        context(
            degenerate.effect.as_ref().unwrap(),
            source.effect_to_source,
            source.size,
            source.size
        )
        .is_none()
    );
    degenerate = parent;
    degenerate.effect.as_mut().unwrap().bounds.x1 = degenerate.effect.as_ref().unwrap().bounds.x0;
    let source = layout(&degenerate);
    assert_eq!(source.effect_to_source, Affine::scale(0.));
    assert_eq!(source.frame_to_source, Affine::scale(0.));
}

#[cfg(feature = "profiling")]
#[test]
fn diagnostic_density_preserves_authored_domain_and_nested_actual_grid() {
    use super::raster_scale::testing::with_enabled;
    let parent = group(
        Affine::translate((17.3, -9.7))
            * Affine::rotate(0.37)
            * Affine::scale_non_uniform(0.837, 1.213),
    );
    let exact = with_enabled(false, || layout(&parent));
    let selected = with_enabled(true, || layout(&parent));
    assert!(selected.size.iter().zip(exact.size).all(|(&a, b)| a >= b));
    assert_ne!(selected.size, exact.size);
    let effect = parent.effect.as_ref().unwrap();
    let before = context(effect, exact.effect_to_source, exact.size, exact.size).unwrap();
    let after = context(
        effect,
        selected.effect_to_source,
        selected.size,
        selected.size,
    )
    .unwrap();
    assert_eq!(
        before[4], after[4],
        "authored Position and Size remain unchanged"
    );
    assert_eq!(after[5][..2], selected.size.map(|n| n as f32));
    let child = parent.clone();
    assert!(same_source_domain(&child, Some(&parent)));
    assert_eq!(
        output_transform(
            &child,
            Some((&parent, selected.effect_to_source)),
            selected.frame_to_source
        ),
        selected.effect_to_source
    );
}
