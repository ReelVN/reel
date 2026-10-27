//! Storage admission contracts use geometry fixtures without GPU readback.
use super::*;
use crate::test_support::r01_fixture;
use reel_core::identity::FrameId;
use reel_ui::{
    frame::{FrameBuild, FrameInfo, InkBounds},
    imaging::{
        Composite,
        record::{Clip, Draw, Geometry, Group, Scene},
    },
    kurbo::Rect,
    peniko::{Brush, Color},
};

struct NoGlyphs;
impl crate::common::glyph_plan::Prepare for NoGlyphs {
    fn reserve(&mut self, _: u64) -> Result<(), GpuError> {
        Ok(())
    }
    fn run(
        &mut self,
        _: &FrameDrawing,
        _: &reel_ui::frame::FrameGlyphRun,
        _: f64,
        _: reel_ui::frame::RecordLocation,
    ) -> Result<InkBounds, GpuError> {
        panic!("geometry-only fixture");
    }
}

#[test]
fn effect_chains_forward_the_complete_source_and_keep_two_scratch_targets() {
    use reel_core::{
        budget::OwnedBytes,
        identity::{AssetKey, ContentVersion, ResourceVersion},
    };
    use reel_ui::{
        core::{PaintEffect, Widget},
        kurbo::{Point, Size},
        resources::{EffectShaderDescriptor, EffectShaderFormat, ResourceRegistry},
        stage::effect::*,
    };
    let pool = r01_fixture::pool();
    let program = EffectProgram::new(
        vec![],
        vec![
            EffectNode {
                ty: EffectType::Vec2,
                op: EffectOp::Input(EffectInput::Uv),
            },
            EffectNode {
                ty: EffectType::Vec4,
                op: EffectOp::SourceSample {
                    uv: EffectNodeId(0),
                },
            },
        ],
        EffectNodeId(1),
    )
    .unwrap();
    let fragment =
        crate::effect::compile_fragment(&program, crate::effect::Language::GlslEs100, 16384)
            .unwrap();
    let descriptor = EffectShaderDescriptor::with_textures(
        [4; 32],
        EffectShaderFormat::GlslEs100,
        vec![],
        0,
        true,
        EffectType::Vec4,
        fragment.source.as_bytes(),
    )
    .unwrap();
    let mut registry = ResourceRegistry::new(1, 16384).unwrap();
    let handle = registry
        .register_effect_shader(
            ResourceVersion {
                key: AssetKey("effect-chain".into()),
                version: ContentVersion(descriptor.shader_key),
            },
            descriptor,
            OwnedBytes::new(
                fragment.source.clone().into_bytes(),
                pool.reserve(fragment.source.len() as u64).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let paint = PaintEffect {
        instance: EffectInstance::new([4; 32], program, vec![]).unwrap(),
        shader: registry.acquire(handle).unwrap(),
        images: vec![],
        bounds: Rect::new(0., 0., 160., 120.),
        coverage_invert: false,
    };
    for transform in [
        Affine::IDENTITY,
        Affine::translate((13.2, -7.6))
            * Affine::scale_non_uniform(0.83, 0.29)
            * Affine::rotate(2.1),
    ] {
        for depth in [1, 2, 4, 8] {
            for opacity in [1., 0.5] {
                let mut scene = Scene::new();
                scene.draw(Draw::Fill {
                    transform: Affine::IDENTITY,
                    fill_rule: Default::default(),
                    brush: Brush::Solid(Color::WHITE),
                    brush_transform: None,
                    shape: Geometry::Rect(paint.bounds),
                    composite: Default::default(),
                });
                let mut widget = r01_fixture::RecordWidget::new(scene);
                widget.clip = false;
                widget.effect = Some(paint.clone());
                widget.opacity = opacity;
                for _ in 1..depth {
                    let mut parent = r01_fixture::RecordWidget::new(Scene::new());
                    parent.clip = false;
                    parent.effect = Some(paint.clone());
                    parent.opacity = opacity;
                    parent
                        .inner
                        .add(widget.prepare(), Point::ZERO, Size::new(160., 120.));
                    widget = parent;
                }
                let mut bridge = r01_fixture::widget_bridge(widget, transform);
                let mut build = FrameBuild::new(
                    bridge.record(FrameId(1)).unwrap(),
                    registry.freeze(&[handle], &pool).unwrap(),
                    FrameInfo {
                        logical_size: [160., 120.],
                        physical_size: [160, 120],
                    },
                    r01_fixture::limits(),
                    &pool,
                )
                .unwrap();
                while !build.step(r01_fixture::step_budget()).unwrap().complete {}
                let frame = build.finish().unwrap();
                for masks in [MaskExecution::Inline, MaskExecution::Sampled] {
                    let pixels = target_pixels(
                        &frame,
                        None,
                        [160, 120],
                        false,
                        1,
                        4096,
                        &crate::common::glyph_plan::Plan::default(),
                        &offscreen::Plan::default(),
                        masks,
                        &Default::default(),
                        |_| Ok(()),
                    )
                    .unwrap();
                    let expected = if transform == Affine::IDENTITY {
                        160 * 120 * if opacity == 1. { 3 } else { depth + 2 }
                    } else {
                        let [a, b, c, d, _, _] = transform.as_coeffs();
                        let local =
                            (160. * a.hypot(b)).ceil() as u64 * (120. * c.hypot(d)).ceil() as u64;
                        let count = if depth == 1 {
                            1
                        } else if opacity == 1. {
                            2
                        } else {
                            depth + 1
                        };
                        2 * 160 * 120 + count * local
                    };
                    assert_eq!(
                        pixels, expected,
                        "depth={depth} opacity={opacity} transform={transform:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn scalar_masks_keep_two_local_sources_and_project_plain_groups_into_the_parent() {
    use reel_core::budget::OwnedBytes;
    use reel_core::identity::{AssetKey, ContentVersion, ResourceVersion};
    use reel_ui::{
        core::PaintEffect,
        resources::{EffectShaderDescriptor, EffectShaderFormat, ResourceRegistry},
        stage::effect::*,
    };
    let size = [160, 120];
    let pool = r01_fixture::pool();
    let program = EffectProgram::new(
        vec![],
        vec![EffectNode {
            ty: EffectType::Float,
            op: EffectOp::Constant(EffectValue::Float(0.5_f32.to_bits())),
        }],
        EffectNodeId(0),
    )
    .unwrap();
    let fragment =
        crate::effect::compile_fragment(&program, crate::effect::Language::GlslEs100, 16384)
            .unwrap();
    let descriptor = EffectShaderDescriptor::with_textures(
        [3; 32],
        EffectShaderFormat::GlslEs100,
        vec![],
        0,
        true,
        EffectType::Float,
        fragment.source.as_bytes(),
    )
    .unwrap();
    let mut registry = ResourceRegistry::new(1, 16384).unwrap();
    let handle = registry
        .register_effect_shader(
            ResourceVersion {
                key: AssetKey("effect-mask".into()),
                version: ContentVersion(descriptor.shader_key),
            },
            descriptor,
            OwnedBytes::new(
                fragment.source.clone().into_bytes(),
                pool.reserve(fragment.source.len() as u64).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let paint = PaintEffect {
        instance: EffectInstance::new([3; 32], program, vec![]).unwrap(),
        shader: registry.acquire(handle).unwrap(),
        images: vec![],
        bounds: Rect::new(0., 0., 240., 80.),
        coverage_invert: true,
    };
    for (clip, invert, target_count) in [(false, 0., 3), (false, 0.5, 4), (true, 0., 5)] {
        for color_effect in [false, true] {
            let mut scene = Scene::new();
            scene.draw(Draw::Fill {
                transform: Affine::IDENTITY,
                fill_rule: Default::default(),
                brush: Brush::Solid(Color::WHITE),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(0., 0., 240., 80.)),
                composite: Default::default(),
            });
            let mut widget = r01_fixture::RecordWidget::new(scene);
            widget.clip = clip;
            widget.inner.clip = Rect::new(0., 0., 80., 70.);
            widget.opacity = 0.75;
            widget.invert = invert;
            widget.mask_effect = Some(paint.clone());
            widget.effect = color_effect.then(|| paint.clone());
            let mut bridge = r01_fixture::widget_bridge(
                widget,
                Affine::translate((-40., -18.)) * Affine::rotate(0.1),
            );
            let mut build = FrameBuild::new(
                bridge.record(FrameId(1)).unwrap(),
                registry.freeze(&[handle], &pool).unwrap(),
                FrameInfo {
                    logical_size: size.map(f64::from),
                    physical_size: size,
                },
                r01_fixture::limits(),
                &pool,
            )
            .unwrap();
            while !build.step(r01_fixture::step_budget()).unwrap().complete {}
            let frame = build.finish().unwrap();
            for masks in [MaskExecution::Inline, MaskExecution::Sampled] {
                let pixels = target_pixels(
                    &frame,
                    None,
                    size,
                    false,
                    1,
                    4096,
                    &crate::common::glyph_plan::Plan::default(),
                    &offscreen::Plan::default(),
                    masks,
                    &Default::default(),
                    |_| Ok(()),
                )
                .unwrap();
                assert_eq!(
                    pixels,
                    target_count * 160 * 120,
                    "clip={clip}, invert={invert}, color={color_effect}"
                );
            }
            let group = frame
                .commands()
                .iter()
                .find_map(|command| match &command.op {
                    FrameOp::PushGroup(group) if group.mask_effect.is_some() => Some(group),
                    _ => None,
                })
                .unwrap();
            assert!(
                !crate::common::effect_plan::direct_mask_projection(group, true),
                "retained transition dependencies retain projection storage"
            );
        }
    }
}

#[test]
fn finite_group_admission_matches_clipped_physical_storage_for_every_sample_tier() {
    let size = [960, 544];
    let pool = r01_fixture::pool();
    for blend in [
        reel_ui::frame::BlendMode::default(),
        reel_ui::peniko::Mix::Screen.into(),
        reel_ui::peniko::Mix::Multiply.into(),
    ] {
        for clipped in [false, true] {
            let mut scene = Scene::new();
            if clipped {
                scene.push_clip(Clip::Fill {
                    transform: Affine::IDENTITY,
                    shape: Geometry::Rect(Rect::new(50., 40., 140., 60.)),
                    fill_rule: Default::default(),
                });
            }
            scene.push_group(Group {
                composite: Composite {
                    alpha: 0.5,
                    blend: blend.imaging().unwrap(),
                },
                ..Default::default()
            });
            for left in [0., 40.] {
                scene.draw(Draw::Fill {
                    transform: Affine::translate((20., 30.)),
                    fill_rule: Default::default(),
                    brush: Brush::Solid(Color::WHITE),
                    brush_transform: None,
                    shape: Geometry::Rect(Rect::new(left, 0., left + 100., 40.)),
                    composite: Default::default(),
                });
            }
            scene.pop_group();
            if clipped {
                scene.pop_clip();
            }
            let mut widget = r01_fixture::RecordWidget::new(scene);
            widget.clip = false;
            let mut bridge = r01_fixture::widget_bridge(widget, Affine::IDENTITY);
            let mut build = FrameBuild::new(
                bridge.record(FrameId(1)).unwrap(),
                r01_fixture::empty_snapshot(&pool),
                FrameInfo {
                    logical_size: size.map(f64::from),
                    physical_size: size,
                },
                r01_fixture::limits(),
                &pool,
            )
            .unwrap();
            while !build.step(r01_fixture::step_budget()).unwrap().complete {}
            let frame = build.finish().unwrap();
            let plan = crate::common::glyph_plan::Plan::collect(
                std::iter::once(&frame),
                Affine::IDENTITY,
                4096,
                false,
                &Default::default(),
                &mut NoGlyphs,
            )
            .unwrap();
            // Union [20,30]..[160,70], one-pixel ink AA, one-pixel
            // conservative raster margin, then a 32-physical-pixel storage bucket.
            let source_pixels = if clipped { 96 * 32 } else { 160 * 64 };
            for samples in [1, 4] {
                let pixels = target_pixels(
                    &frame,
                    None,
                    size,
                    false,
                    samples,
                    4096,
                    &plan,
                    &offscreen::Plan::default(),
                    MaskExecution::Inline,
                    &Default::default(),
                    |_| Ok(()),
                )
                .unwrap();
                assert_eq!(
                    pixels,
                    u64::from(size[0])
                        * u64::from(size[1])
                        * if blend == Default::default() { 1 } else { 2 }
                        + source_pixels
                );
            }
        }
    }
}

fn sibling_frame(barrier: bool, excluded: bool) -> RenderFrame {
    let mut scene = Scene::new();
    let draw = |scene: &mut Scene, left| {
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Solid(Color::WHITE),
            brush_transform: None,
            shape: Geometry::Rect(Rect::new(left, 20., left + 80., 80.)),
            composite: Default::default(),
        })
    };
    for index in 0..8 {
        if barrier && index == 3 {
            draw(&mut scene, 600.);
        }
        scene.push_group(Group::default());
        scene.push_group(Group {
            composite: Composite {
                alpha: 0.5,
                ..Default::default()
            },
            ..Default::default()
        });
        draw(&mut scene, 20. + index as f64 * 60.);
        draw(&mut scene, 60. + index as f64 * 60.);
        scene.pop_group();
        scene.pop_group();
    }
    let pool = r01_fixture::pool();
    let mut widget = r01_fixture::RecordWidget::new(scene);
    widget.clip = false;
    widget.capture_included = !excluded;
    let mut bridge = r01_fixture::widget_bridge(widget, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(10)).unwrap(),
        r01_fixture::empty_snapshot(&pool),
        FrameInfo {
            logical_size: [960., 544.],
            physical_size: [960, 544],
        },
        r01_fixture::limits(),
        &pool,
    )
    .unwrap();
    while !build.step(r01_fixture::step_budget()).unwrap().complete {}
    build.finish().unwrap()
}

#[test]
fn sibling_admission_counts_all_live_sources_and_keeps_original_composite_order() {
    let frame = sibling_frame(false, false);
    let glyphs = crate::common::glyph_plan::Plan::default();
    let root = 960 * 544;
    let source = 128 * 64;
    for masks in [MaskExecution::Inline, MaskExecution::Sampled] {
        for width in [1, 2, 4, 8] {
            let limit = root + width * source;
            let (schedule, pixels, _) = offscreen::Plan::choose(
                &frame,
                None,
                [960, 544],
                false,
                1,
                4096,
                &glyphs,
                limit,
                masks,
                &Default::default(),
                |_| Ok(()),
            )
            .unwrap();
            assert_eq!(pixels, limit);
            let entries: Vec<_> = (0..frame.commands().len())
                .filter_map(|index| schedule.entry(index))
                .collect();
            if width == 1 {
                assert!(entries.is_empty());
                continue;
            }
            assert_eq!(entries.len(), 8);
            assert_eq!(
                entries.iter().filter(|entry| entry.last).count(),
                8 / width as usize
            );
            assert!(entries.windows(2).all(|pair| pair[0].push < pair[1].push
                && pair[0].view.origin()[0] < pair[1].view.origin()[0]));
            assert!(
                entries
                    .iter()
                    .all(|entry| entry.view.storage_size == [128, 64])
            );
        }
    }
}

#[test]
fn root_draws_delimit_batches_and_capture_exclusions_keep_their_own_stream() {
    let glyphs = crate::common::glyph_plan::Plan::default();
    let frame = sibling_frame(true, false);
    let (schedule, _, _) = offscreen::Plan::choose(
        &frame,
        None,
        [960, 544],
        false,
        1,
        4096,
        &glyphs,
        u64::MAX,
        MaskExecution::Inline,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    let entries: Vec<_> = (0..frame.commands().len())
        .filter_map(|index| schedule.entry(index))
        .collect();
    assert_eq!(entries.len(), 8);
    assert_eq!(
        entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.last.then_some(i))
            .collect::<Vec<_>>(),
        [2, 7]
    );
    let (narrow, _, _) = offscreen::Plan::choose(
        &frame,
        None,
        [960, 544],
        false,
        1,
        4096,
        &glyphs,
        960 * 544 + 2 * 128 * 64,
        MaskExecution::Inline,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    let scheduled: Vec<_> = (0..frame.commands().len())
        .filter_map(|index| narrow.entry(index))
        .collect();
    assert_eq!(scheduled.len(), 6);
    assert_eq!(scheduled.iter().filter(|entry| entry.last).count(), 3);
    assert!(narrow.entry(entries[2].push).is_none());
    assert!(narrow.entry(entries[7].push).is_none());
    let excluded = sibling_frame(false, true);
    let (schedule, pixels, _) = offscreen::Plan::choose(
        &excluded,
        None,
        [960, 544],
        true,
        1,
        4096,
        &glyphs,
        u64::MAX,
        MaskExecution::Inline,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    assert!(schedule.is_empty());
    assert_eq!(pixels, 960 * 544);
}

#[test]
fn atlas_regions_preserve_parent_samples_and_share_one_accounted_surface() {
    let frame = sibling_frame(false, false);
    let (plan, pixels, _) = offscreen::Plan::choose(
        &frame,
        None,
        [960, 544],
        false,
        1,
        4096,
        &crate::common::glyph_plan::Plan::default(),
        u64::MAX,
        MaskExecution::Inline,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    let entries: Vec<_> = (0..frame.commands().len())
        .filter_map(|i| plan.entry(i))
        .collect();
    assert_eq!(entries.len(), 8);
    assert_eq!(entries.iter().filter(|e| e.first).count(), 1);
    assert_eq!(entries.iter().filter(|e| e.last).count(), 1);
    let size = entries[0].view.storage_size;
    assert_eq!(size, [128, 544]);
    assert_eq!(pixels, 960 * 544 + u64::from(size[0]) * u64::from(size[1]));
    for (index, entry) in entries.iter().enumerate() {
        assert!(entry.shared);
        assert_eq!(entry.view.storage_size, size);
        let rect = entry.view.source_rect;
        assert!(rect.x > 0 && rect.y > 0);
        assert!(rect.x + rect.width < size[0] && rect.y + rect.height < size[1]);
        for other in &entries[..index] {
            assert!(rect.intersect(other.view.source_rect).empty());
        }
        let origin = entry.view.origin();
        let uv = crate::common::source_view::uv_mapping([960, 544], size, origin);
        for (x, y) in [
            (0.5, 0.5),
            (f64::from(rect.width) - 0.5, f64::from(rect.height) - 0.5),
        ] {
            let parent = reel_ui::kurbo::Point::new(
                f64::from(entry.view.parent_rect.x) + x,
                f64::from(entry.view.parent_rect.y) + y,
            );
            let source = entry.view.parent_to_source * parent;
            assert_eq!(source.x, f64::from(rect.x) + x);
            assert_eq!(source.y, f64::from(rect.y) + y);
            for (axis, (p, s)) in [(parent.x, source.x), (parent.y, source.y)]
                .into_iter()
                .enumerate()
            {
                let sampled =
                    (p as f32 / [960., 544.][axis] * uv[axis] + uv[axis + 2]) * size[axis] as f32;
                assert!((sampled - s as f32).abs() < 0.0001);
            }
        }
    }
    assert!(entries.iter().any(|e| e.view.origin()[1] < 0));
}

#[test]
fn narrowing_releases_completed_traversal_reservations() {
    use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
    let frame = sibling_frame(false, false);
    let glyphs = crate::common::glyph_plan::Plan::default();
    let plan = |pool: &BudgetPool, source_count: u64| {
        offscreen::Plan::choose(
            &frame,
            None,
            [960, 544],
            false,
            1,
            4096,
            &glyphs,
            960 * 544 + source_count * 128 * 64,
            MaskExecution::Inline,
            &Default::default(),
            |bytes| {
                pool.reserve(bytes).map_err(|_| GpuError::Budget {
                    domain: "test planning",
                    requested: bytes,
                    available: 0,
                })
            },
        )
    };
    let generous = BudgetPool::new(
        BudgetCategory::Temporary,
        QueueLimits {
            max_requests: u32::MAX,
            max_bytes: u64::MAX,
        },
    );
    let (schedule, _, retained) = plan(&generous, 8).unwrap();
    let peak = generous.usage().peak_bytes;
    assert!(generous.usage().bytes > 0);
    drop(schedule);
    drop(retained);
    assert_eq!(generous.usage().bytes, 0);
    let tight = BudgetPool::new(
        BudgetCategory::Temporary,
        QueueLimits {
            max_requests: u32::MAX,
            max_bytes: peak,
        },
    );
    let (schedule, pixels, retained) = plan(&tight, 1).unwrap();
    assert!(schedule.is_empty());
    assert_eq!(pixels, 960 * 544 + 128 * 64);
    assert!(retained.is_empty());
    assert_eq!(tight.usage().bytes, 0);
}
