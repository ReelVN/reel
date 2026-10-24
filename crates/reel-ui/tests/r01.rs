#[allow(dead_code, reason = "shared real U02 and R01 fixtures")]
#[path = "support/fixture.rs"]
mod fixture;
#[allow(dead_code, reason = "shared real R01 fixtures")]
#[path = "support/r01_fixture.rs"]
mod r01_fixture;
use r01_fixture::*;
use reel_core::{
    budget::BudgetPool,
    identity::{AssetKey, ContentVersion, FrameId, SurfaceEpoch},
};
use reel_ui::{bridge::*, frame::*, resources::*};
use reel_ui::{
    core::*,
    imaging::{
        Composite, MaskMode, SourceLocationRef,
        record::{self, AppliedMask, Clip, Draw, Geometry, Group, Mask, Scene},
    },
    kurbo::{Affine, BezPath, Point, Rect, Size, Stroke, Vec2},
    peniko::{self, Blob, Brush, Color, FontData, Gradient},
};
use std::sync::Arc;
fn fill(color: Color) -> Scene {
    let mut s = Scene::new();
    s.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: peniko::Fill::NonZero,
        brush: Brush::Solid(color),
        brush_transform: None,
        shape: Geometry::Rect(Rect::new(0., 0., 20., 20.)),
        composite: Composite::default(),
    });
    s
}
fn version(n: u8) -> ResourceVersion {
    ResourceVersion {
        key: AssetKey(format!("resource/{n}")),
        version: ContentVersion([n; 32]),
    }
}

#[test]
fn expired_registration_reclaims_its_descriptor_before_slot_reuse() {
    let pool = pool();
    let mut probe = ResourceRegistry::new(1, 8192).unwrap();
    probe
        .register(
            version(1),
            ResourceData::External { token: 1 },
            ticket(&pool, 0),
        )
        .unwrap();
    let exact_capacity = probe.usage().bytes;
    drop(probe);
    for external_weak in [false, true] {
        let mut registry = ResourceRegistry::new(1, exact_capacity).unwrap();
        let old = registry
            .register(
                version(1),
                ResourceData::External { token: 1 },
                ticket(&pool, 0),
            )
            .unwrap();
        let weak = external_weak.then(|| registry.acquire(old).unwrap().downgrade());
        registry.retire(old);
        if external_weak {
            assert_eq!(
                registry.register(
                    version(2),
                    ResourceData::External { token: 2 },
                    ticket(&pool, 0)
                ),
                Err(ResourceError::Bytes)
            );
        }
        drop(weak);
        let new = registry
            .register(
                version(2),
                ResourceData::External { token: 2 },
                ticket(&pool, 0),
            )
            .expect("released registration must not consume the replacement descriptor quota");
        assert_eq!(new.slot, old.slot);
        assert!(new.generation > old.generation);
        assert!(registry.acquire(old).is_err());
        assert_eq!(registry.usage().bytes, exact_capacity);
    }
}

#[test]
fn inline_child_draws_and_errors_keep_the_actual_command_owner() {
    let pool = pool();
    for case in 0..5 {
        let mut own = fill(Color::WHITE);
        if case == 1 {
            own = Scene::new();
            own.draw(Draw::Stroke {
                transform: Affine::IDENTITY,
                stroke: Stroke::new(-1.),
                brush: Brush::Solid(Color::WHITE),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(0., 0., 10., 10.)),
                composite: Composite::default(),
            });
        } else if case == 2 {
            own = Scene::new();
            own.draw(Draw::Fill {
                transform: Affine::new([f64::NAN, 0., 0., 1., 0., 0.]),
                fill_rule: Default::default(),
                brush: Brush::Solid(Color::WHITE),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(0., 0., 10., 10.)),
                composite: Composite::default(),
            });
        } else if case == 3 {
            own = Scene::new();
            own.draw(Draw::Fill {
                transform: Affine::IDENTITY,
                fill_rule: Default::default(),
                brush: Brush::Image(peniko::ImageBrush::new(fixture::image_data([
                    255, 0, 0, 255,
                ]))),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(0., 0., 10., 10.)),
                composite: Composite::default(),
            });
        } else if case == 4 {
            let mut path = BezPath::with_capacity(8194);
            path.move_to((0., 0.));
            for i in 0..8192 {
                path.line_to((i as f64, 0.));
            }
            path.line_to((f64::NAN, 0.));
            own = Scene::new();
            own.draw(Draw::Fill {
                transform: Affine::IDENTITY,
                fill_rule: Default::default(),
                brush: Brush::Solid(Color::WHITE),
                brush_transform: None,
                shape: Geometry::Path(path),
                composite: Composite::default(),
            });
        }
        let mut parent = RecordWidget::new(fill(Color::BLACK));
        let child = parent.inner.add(
            RecordWidget::new(own).prepare(),
            Point::new(13., 19.),
            Size::new(40., 30.),
        );
        let mut bridge = widget_bridge(parent, Affine::IDENTITY);
        let source = bridge.record(FrameId(1)).unwrap();
        assert_eq!(
            source.plan().layers.len(),
            1,
            "ordinary Inline child shares a fragment"
        );
        if case == 0 {
            let frame = build(source, empty_snapshot(&pool), &pool);
            let owners: Vec<_> = frame
                .commands()
                .iter()
                .filter_map(|c| matches!(c.op, FrameOp::Draw(_)).then_some(c.origin.at.owner))
                .collect();
            assert_eq!(owners.last(), Some(&child));
        } else {
            let notice = PresentationNotice::new(
                source.key(),
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(FrameId(1)),
            );
            let mut build = FrameBuild::new(
                source,
                empty_snapshot(&pool),
                FrameInfo {
                    logical_size: [400., 300.],
                    physical_size: [400, 300],
                },
                limits(),
                &pool,
            )
            .unwrap();
            let error = loop {
                match build.step(step_budget()) {
                    Err(e) => break e,
                    Ok(p) => assert!(!p.complete, "invalid record unexpectedly completed"),
                }
            };
            assert_eq!(
                error.at.owner, child,
                "case {case}: Inline child failure belongs to its painter"
            );
            assert_eq!(error.at.fragment, 0);
            if case == 3 {
                assert!(matches!(error.kind, BuildErrorKind::MissingResource { .. }));
            } else {
                assert_eq!(error.kind, BuildErrorKind::InvalidRecord);
            }
            assert_eq!(bridge.pending_count(), 0);
            assert_eq!(
                bridge.resolve(notice),
                NoticeResult::StaleOrForeign,
                "a build failure invalidates even an outstanding notice"
            );
        }
    }
}

#[test]
fn debug_bounds_are_drawn_once_in_window_space_even_when_singular() {
    if std::env::var_os("REEL_R01_DEBUG_CHILD").is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "debug_bounds_are_drawn_once_in_window_space_even_when_singular",
                "--nocapture",
            ])
            .env("REEL_R01_DEBUG_CHILD", "1")
            .env("MASONRY_DEBUG_PAINT", "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let pool = pool();
    for transform in [
        Affine::translate((17., 23.)) * Affine::scale_non_uniform(2., 3.),
        Affine::new([0., 0., 0., 2., 17., 23.]),
    ] {
        let mut parent = RecordWidget::new(Scene::new());
        parent.scroll = Vec2::new(-7., -11.);
        let child = parent.inner.add(
            RecordWidget::new(Scene::new()).prepare(),
            Point::new(13., 19.),
            Size::new(40., 30.),
        );
        let mut bridge = widget_bridge(parent, transform);
        let frame = build(
            bridge.record(FrameId(1)).unwrap(),
            empty_snapshot(&pool),
            &pool,
        );
        let actual: Vec<_> = frame
            .commands()
            .iter()
            .filter_map(|c| match &c.op {
                FrameOp::Draw(FrameDraw::Stroke {
                    transform,
                    shape: Geometry::Rect(rect),
                    ..
                }) => Some((*rect, *transform)),
                _ => None,
            })
            .collect();
        assert_eq!(actual.len(), 2);
        let expected: Vec<_> = frame
            .geometry()
            .records()
            .iter()
            .rev()
            .map(|r| r.bounding_box.inset(-0.5))
            .collect();
        for ((rect, transform), expected) in actual.into_iter().zip(expected) {
            affine_agrees(transform, Affine::IDENTITY);
            assert_eq!(
                rect, expected,
                "window-space debug geometry is never transformed twice"
            );
        }
        assert!(frame.geometry().get(child).is_some());
    }
}
fn ticket(pool: &BudgetPool, n: usize) -> reel_core::budget::BudgetTicket {
    pool.reserve(n as u64).unwrap()
}

#[test]
fn small_work_slices_keep_inline_child_ownership() {
    let pool = pool();
    for (slice_units, parent_draw) in [(1, true), (64, true), (1, false), (64, false)] {
        let mut parent = RecordWidget::new(if parent_draw {
            fill(Color::BLACK)
        } else {
            Scene::new()
        });
        parent.clip = false;
        let mut child = RecordWidget::new(fill(Color::WHITE));
        child.clip = false;
        let child_id = parent
            .inner
            .add(child.prepare(), Point::new(13., 19.), Size::new(40., 30.));
        let parent = parent.prepare();
        let parent_id = parent.id();
        let mut bridge = bridge(fixture::root(parent).0);
        let mut build = FrameBuild::new(
            bridge.record(FrameId(1)).unwrap(),
            empty_snapshot(&pool),
            FrameInfo {
                logical_size: [400., 300.],
                physical_size: [400, 300],
            },
            limits(),
            &pool,
        )
        .unwrap();
        let mut slice = StepBudget {
            max_commands: 1,
            max_units: slice_units,
            max_bytes: 4096,
        };
        let mut complete = false;
        for _ in 0..256 {
            let progress = build.step(slice).unwrap();
            assert!(
                progress.units <= slice.max_units
                    && progress.bytes <= slice.max_bytes
                    && progress.commands <= 1
            );
            if progress.complete {
                complete = true;
                break;
            }
            slice.max_units = slice.max_units.max(progress.minimum_units);
            slice.max_bytes = slice.max_bytes.max(progress.minimum_bytes);
        }
        assert!(complete);
        let frame = build.finish().unwrap();
        let expected = if parent_draw {
            vec![parent_id, child_id]
        } else {
            vec![child_id]
        };
        assert_eq!(
            frame
                .commands()
                .iter()
                .map(|c| c.origin.at.owner)
                .collect::<Vec<_>>(),
            expected
        );
        drop(frame);
        assert_eq!(pool.usage().bytes, 0);
        assert_eq!(bridge.pending_count(), 0);
    }
}
fn affine_agrees(actual: Affine, expected: Affine) {
    for (a, e) in actual.as_coeffs().into_iter().zip(expected.as_coeffs()) {
        assert!((a - e).abs() < 1e-12, "{actual:?} != {expected:?}");
    }
}
#[test]
fn source_drop_cancel_epoch_and_success_keep_the_exact_acceptance_gate() {
    let mut bridge = widget_bridge(RecordWidget::new(fill(Color::WHITE)), Affine::IDENTITY);
    let pool = pool();
    let source = bridge.record(FrameId(1)).unwrap();
    let key = source.key();
    let outstanding = PresentationNotice::new(
        key.clone(),
        SurfaceEpoch(1),
        PresentationOutcome::Accepted(FrameId(1)),
    );
    let mut b = FrameBuild::new(
        source,
        empty_snapshot(&pool),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        limits(),
        &pool,
    )
    .unwrap();
    b.step(StepBudget {
        max_commands: 1,
        max_units: 1,
        max_bytes: 4096,
    })
    .unwrap();
    b.cancel();
    assert_eq!(bridge.pending_count(), 0);
    assert_eq!(pool.usage().bytes, 0);
    assert_eq!(bridge.resolve(outstanding), NoticeResult::StaleOrForeign);
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            key,
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(1))
        )),
        NoticeResult::StaleOrForeign
    );
    let source = bridge.record(FrameId(2)).unwrap();
    let mut b = FrameBuild::new(
        source,
        empty_snapshot(&pool),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        limits(),
        &pool,
    )
    .unwrap();
    bridge.replace_surface(SurfaceEpoch(2));
    assert_eq!(
        b.step(step_budget()).unwrap_err().kind,
        BuildErrorKind::Cancelled
    );
    assert_eq!(pool.usage().bytes, 0);
    let frame = build(
        bridge.record(FrameId(3)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    assert!(bridge.accepted_key().is_none());
    assert_eq!(bridge.pending_count(), 1);
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            frame.key(),
            SurfaceEpoch(2),
            PresentationOutcome::Accepted(FrameId(99))
        )),
        NoticeResult::StaleOrForeign
    );
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            frame.key(),
            SurfaceEpoch(2),
            PresentationOutcome::Accepted(frame.frame_id())
        )),
        NoticeResult::Accepted
    );
    let source = bridge.record(FrameId(4)).unwrap();
    let key = source.key();
    let mut b = FrameBuild::new(
        source,
        empty_snapshot(&pool),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        limits(),
        &pool,
    )
    .unwrap();
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            key,
            SurfaceEpoch(2),
            PresentationOutcome::Error
        )),
        NoticeResult::NotPresented
    );
    assert_eq!(
        b.step(step_budget()).unwrap_err().kind,
        BuildErrorKind::Cancelled
    );
    assert_eq!(bridge.accepted_key().unwrap().frame_id(), FrameId(3));
}
#[test]
fn cross_fragment_groups_external_order_cache_and_forward_scroll_singular() {
    for singular in [false, true] {
        let mut parent = RecordWidget::new(Scene::new());
        parent.pre.push_group(Group {
            composite: Composite {
                alpha: 0.5,
                ..Composite::default()
            },
            ..Group::default()
        });
        parent.post.pop_group();
        parent.scroll = Vec2::new(-5., -7.);
        let mut external = RecordWidget::new(fill(Color::BLACK));
        external.mode = PaintLayerMode::External;
        external.token = 42;
        external.pre = fill(Color::from_rgb8(255, 0, 0));
        external.post = fill(Color::from_rgb8(0, 0, 255));
        external.scroll = Vec2::new(-3., -4.);
        let child = RecordWidget::new(fill(Color::from_rgb8(0, 255, 0))).prepare();
        let child_id = child.id();
        external
            .inner
            .add(child, Point::new(15., 20.), Size::new(100., 80.));
        let ext = external
            .prepare()
            .with_props(
                PropertySet::new().with(reel_ui::properties::Padding::from_vh(
                    reel_ui::layout::Length::px(6.),
                    reel_ui::layout::Length::px(9.),
                )),
            )
            .with_transform(Affine::scale_non_uniform(2., 3.));
        let ext_id = ext.id();
        parent
            .inner
            .add(ext, Point::new(30., 40.), Size::new(150., 100.));
        let transform = Affine::translate((70., 50.))
            * Affine::scale_non_uniform(if singular { 0. } else { 1.5 }, 1.2);
        let mut bridge = widget_bridge(parent, transform);
        let pool = pool();
        let mut registry = ResourceRegistry::new(1, 4096).unwrap();
        let h = registry
            .register(
                version(42),
                ResourceData::External { token: 42 },
                ticket(&pool, 0),
            )
            .unwrap();
        for frame_id in 1..=3 {
            if frame_id == 3 {
                bridge.edit_widget(ext_id, |mut w| w.ctx.request_pre_paint());
            }
            let source = bridge.record(FrameId(frame_id)).unwrap();
            let geometry_child = source.geometry().get(child_id).unwrap().window_transform;
            let ext_geometry = source.geometry().get(ext_id).unwrap();
            let geometry_ext = ext_geometry.window_transform.pre_translate(Vec2::new(
                ext_geometry.border_box_insets.x0,
                ext_geometry.border_box_insets.y0,
            ));
            let ext_size = Size::new(
                ext_geometry.size.width - ext_geometry.border_box_insets.x_value(),
                ext_geometry.size.height - ext_geometry.border_box_insets.y_value(),
            );
            let frame = build(source, registry.freeze(&[h], &pool).unwrap(), &pool);
            let ops: Vec<_> = frame
                .commands()
                .iter()
                .map(|c| match &c.op {
                    FrameOp::PushGroup(_) => "group",
                    FrameOp::PopGroup => "endgroup",
                    FrameOp::PushClip(_) => "clip",
                    FrameOp::PopClip => "endclip",
                    FrameOp::External(_) => "external",
                    FrameOp::Draw(_) => "draw",
                    _ => "context",
                })
                .collect();
            assert_eq!(
                ops,
                vec![
                    "group", "clip", "draw", "clip", "external", "clip", "draw", "endclip",
                    "endclip", "draw", "endclip", "endgroup"
                ]
            );
            let ext = frame
                .commands()
                .iter()
                .find_map(|c| {
                    if let FrameOp::External(e) = &c.op {
                        Some(e)
                    } else {
                        None
                    }
                })
                .unwrap();
            assert_eq!(ext.owner, ext_id);
            assert_eq!(ext.token, 42);
            affine_agrees(ext.content_to_window, geometry_ext);
            assert_eq!(ext.local_bounds, ext_size.to_rect());
            assert_eq!(ext.local_bounds.origin(), Point::ORIGIN);
            let child = frame
                .commands()
                .iter()
                .filter_map(|c| {
                    if let FrameOp::Draw(FrameDraw::Fill {
                        transform,
                        brush: Brush::Solid(color),
                        ..
                    }) = &c.op
                    {
                        (*color == Color::from_rgb8(0, 255, 0)).then_some(*transform)
                    } else {
                        None
                    }
                })
                .next()
                .unwrap();
            affine_agrees(child, geometry_child);
            let group = frame
                .commands()
                .iter()
                .find_map(|c| {
                    if let FrameOp::PushGroup(g) = &c.op {
                        Some(g)
                    } else {
                        None
                    }
                })
                .unwrap();
            affine_agrees(group.effect_to_target, transform);
            assert!(frame.geometry().get(group.owner).is_some());
            if singular {
                assert_ne!(
                    frame.geometry().hit_test(Point::new(70., 70.)),
                    Some(child_id)
                );
                assert_ne!(
                    frame.geometry().hit_test(Point::new(70., 70.)),
                    Some(ext_id)
                );
            }
            bridge.resolve(PresentationNotice::new(
                frame.key(),
                SurfaceEpoch(1),
                PresentationOutcome::Skipped,
            ));
        }
        bridge.edit_widget(ext_id, |mut w| {
            let mut w = w.downcast::<RecordWidget>();
            w.widget.mode = PaintLayerMode::Inline;
            w.ctx.request_render();
        });
        let frame = build(
            bridge.record(FrameId(4)).unwrap(),
            empty_snapshot(&pool),
            &pool,
        );
        assert!(
            !frame
                .commands()
                .iter()
                .any(|c| matches!(c.op, FrameOp::External(_)))
        );
    }
}
#[test]
fn portable_profile_rejects_crossed_lifetimes_with_location_and_releases() {
    let mut scene = Scene::new();
    scene.push_clip(Clip::Fill {
        transform: Affine::IDENTITY,
        shape: Geometry::Rect(Rect::new(0., 0., 20., 20.)),
        fill_rule: peniko::Fill::NonZero,
    });
    scene.push_group(Group::default());
    scene.pop_clip();
    scene.pop_group();
    assert!(scene.validate().is_ok());
    let mut bridge = widget_bridge(RecordWidget::new(scene), Affine::IDENTITY);
    let pool = pool();
    let mut b = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        limits(),
        &pool,
    )
    .unwrap();
    let error = loop {
        match b.step(step_budget()) {
            Err(e) => break e,
            Ok(p) => assert!(!p.complete),
        }
    };
    assert_eq!(error.kind, BuildErrorKind::UnsupportedRecord);
    assert_eq!(error.at.mask_depth, 0);
    assert_eq!(bridge.pending_count(), 0);
    assert_eq!(pool.usage().bytes, 0);
}
#[test]
fn mask_definitions_reuse_local_id_and_independent_transform_and_stack() {
    let mut scene = Scene::new();
    let mask = scene.define_mask(Mask::new(MaskMode::Luminance, fill(Color::WHITE)));
    for x in [2., 30.] {
        scene.push_group(Group {
            mask: Some(AppliedMask {
                mask,
                transform: Affine::translate((x, 8.)),
            }),
            ..Group::default()
        });
        scene.pop_group();
    }
    let mut widget = RecordWidget::new(scene);
    widget.pre.push_clip(Clip::Fill {
        transform: Affine::IDENTITY,
        shape: Geometry::Rect(Rect::new(0., 0., 400., 300.)),
        fill_rule: peniko::Fill::NonZero,
    });
    widget.post.pop_clip();
    let mut bridge = widget_bridge(widget, Affine::translate((40., 60.)));
    let pool = pool();
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    assert_eq!(frame.masks().len(), 1);
    let uses: Vec<_> = frame
        .commands()
        .iter()
        .filter_map(|c| {
            if let FrameOp::PushGroup(g) = &c.op {
                g.mask
            } else {
                None
            }
        })
        .collect();
    assert_eq!(uses.len(), 2);
    assert_eq!(uses[0].mask, uses[1].mask);
    assert_eq!(uses[0].transform, Affine::translate((42., 68.)));
    assert_eq!(uses[1].transform, Affine::translate((70., 68.)));
    assert!(matches!(
        frame.masks()[&0].commands[0].op,
        FrameOp::Draw(FrameDraw::Fill {
            transform: Affine::IDENTITY,
            ..
        })
    ));
}
#[test]
fn large_path_gradient_glyph_variation_dash_and_utf8_resume_then_retire() {
    let pool = pool();
    let mut registry = ResourceRegistry::new(2, 8192).unwrap();
    let blob = Blob::new(Arc::new([1u8, 2, 3, 4]));
    let font = FontData::new(blob, 3);
    let handle = registry
        .register(
            version(1),
            ResourceData::Font(font.data.clone()),
            ticket(&pool, 4),
        )
        .unwrap();
    let weak = registry.acquire(handle).unwrap().downgrade();
    assert_eq!(weak.upgrade().unwrap().handle(), handle);
    let mut path = BezPath::new();
    path.move_to((0., 0.));
    for i in 0..8192 {
        path.line_to((i as f64 % 100., (i / 100) as f64));
    }
    let gradient = Gradient::new_linear((0., 0.), (100., 100.)).with_stops(
        (0..2048)
            .map(|i| peniko::ColorStop::from((i as f32 / 2047., Color::WHITE)))
            .collect::<Vec<_>>()
            .as_slice(),
    );
    let stroke = Stroke::new(2.).with_dashes(0., (0..2048).map(|_| 1.));
    let mut scene = Scene::new();
    let label = "图形位置".repeat(2048);
    scene.push_context(
        &label,
        Some(SourceLocationRef::new("record/widget.rs", 12, 9)),
    );
    scene.draw(Draw::Stroke {
        transform: Affine::IDENTITY,
        stroke,
        brush: Brush::Gradient(gradient),
        brush_transform: Some(Affine::scale(2.)),
        shape: Geometry::Path(path),
        composite: Composite::default(),
    });
    let mut glyphs = record::GlyphRun::new(font);
    glyphs.glyphs = (0..8192)
        .map(|id| record::Glyph {
            id,
            x: id as f32,
            y: 2.,
        })
        .collect();
    glyphs.normalized_coords = vec![0; 2048];
    scene.draw(Draw::GlyphRun(glyphs));
    scene.pop_context();
    let mut bridge = widget_bridge(RecordWidget::new(scene), Affine::IDENTITY);
    let source = bridge.record(FrameId(1)).unwrap();
    let frozen = registry.freeze(&[handle], &pool).unwrap();
    registry.retire(handle);
    assert!(
        weak.upgrade().is_none(),
        "explicit retirement closes weak admission"
    );
    assert!(registry.acquire(handle).is_err());
    let mut b = FrameBuild::new(
        source,
        frozen,
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        limits(),
        &pool,
    )
    .unwrap();
    let mut slices = 0;
    loop {
        let p = b.step(step_budget()).unwrap();
        assert!(p.units <= 128 && p.bytes <= 4096 && p.commands <= 2);
        slices += 1;
        if p.complete {
            break;
        }
        assert!(
            p.units > 0 || p.bytes > 0,
            "payload copying or prepaid capacity must advance"
        );
        assert!(slices < 10000);
    }
    let frame = b.finish().unwrap();
    assert!(slices > 100);
    assert!(!weak.is_released());
    assert!(bridge.accepted_key().is_none());
    let c = frame
        .commands()
        .iter()
        .find_map(|c| {
            if let FrameOp::PushContext(c) = &c.op {
                Some(c)
            } else {
                None
            }
        })
        .unwrap();
    let FrameContextValue::Str(text) = &c.value else {
        panic!()
    };
    assert_eq!(text.as_str(), label);
    let g = frame
        .commands()
        .iter()
        .find_map(|c| {
            if let FrameOp::Draw(FrameDraw::GlyphRun(g)) = &c.op {
                Some(g)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(g.font, handle);
    assert_eq!(g.font_index, 3);
    assert_eq!(g.glyphs.len(), 8192);
    assert_eq!(g.normalized_coords.len(), 2048);
    drop(frame);
    assert!(weak.is_released());
    assert_eq!(bridge.pending_count(), 0);
    assert_eq!(pool.usage().bytes, 0);
    let next = registry
        .register(
            version(2),
            ResourceData::External { token: 7 },
            ticket(&pool, 0),
        )
        .unwrap();
    assert_eq!(next.slot, handle.slot);
    assert!(next.generation > handle.generation);
    assert!(registry.acquire(handle).is_err());
}
#[test]
fn blob_collision_is_not_a_content_version_and_old_registry_handles_are_foreign() {
    let pool = pool();
    let a: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new([1u8, 2, 3, 4]);
    let b: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new([5u8, 6, 7, 8]);
    let fa = FontData::new(Blob::from_raw_parts(a, 9), 0);
    let fb = FontData::new(Blob::from_raw_parts(b, 9), 0);
    let mut r = ResourceRegistry::new(2, 8192).unwrap();
    let h = r
        .register(
            version(1),
            ResourceData::Font(fa.data.clone()),
            ticket(&pool, 4),
        )
        .unwrap();
    let other = r
        .register(
            version(2),
            ResourceData::Font(fb.data.clone()),
            ticket(&pool, 4),
        )
        .unwrap();
    assert_ne!(h, other);
    let again = r
        .register(version(1), ResourceData::Font(fa.data), ticket(&pool, 4))
        .unwrap();
    assert_eq!(h, again);
    assert_eq!(r.usage().tickets, 3);
    let foreign = ResourceRegistry::new(2, 8192).unwrap();
    assert!(foreign.acquire(h).is_err());
    let mut scene = Scene::new();
    scene.draw(Draw::GlyphRun(record::GlyphRun::new(fb)));
    let mut bridge = widget_bridge(RecordWidget::new(scene), Affine::IDENTITY);
    let mut b = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        r.freeze(&[h], &pool).unwrap(),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        limits(),
        &pool,
    )
    .unwrap();
    let e = loop {
        if let Err(e) = b.step(step_budget()) {
            break e;
        }
    };
    assert_eq!(e.kind, BuildErrorKind::MissingResource { blob_id: 9 });
    assert_eq!(bridge.pending_count(), 0);
}
#[test]
fn byte_exhaustion_fails_before_publication() {
    let pool = pool();
    let mut bridge = widget_bridge(RecordWidget::new(fill(Color::WHITE)), Affine::IDENTITY);
    let error = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        BuildLimits { max_bytes: 1 },
        &pool,
    )
    .err()
    .unwrap();
    assert_eq!(error.kind, BuildErrorKind::Bytes);
    assert_eq!(bridge.pending_count(), 0);
    assert_eq!(pool.usage().bytes, 0);
}
#[test]
fn large_scene_and_nested_masks_use_growing_stacks_and_small_work_slices() {
    let mut scene = Scene::new();
    for _ in 0..4097 {
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: peniko::Fill::NonZero,
            brush: Brush::Solid(Color::WHITE),
            brush_transform: None,
            shape: Geometry::Rect(Rect::new(0., 0., 20., 20.)),
            composite: Composite::default(),
        });
    }
    for _ in 0..96 {
        let mut parent = Scene::new();
        let mask = parent.define_mask(Mask::new(MaskMode::Alpha, scene));
        parent.push_group(Group {
            mask: Some(AppliedMask::new(mask)),
            ..Group::default()
        });
        parent.pop_group();
        scene = parent;
    }
    let mut bridge = widget_bridge(RecordWidget::new(scene), Affine::IDENTITY);
    let pool = pool();
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    assert_eq!(frame.masks().len(), 96);
    assert_eq!(
        frame.masks().last_key_value().unwrap().1.commands.len(),
        4097
    );
    assert!(frame.accounted_bytes() <= frame.reserved_bytes());
    drop(frame);
    assert_eq!(bridge.pending_count(), 0);
    assert_eq!(pool.usage().bytes, 0);
}
#[test]
fn ink_bounds_cover_stroke_aa_at_dpi_and_finite_overflow_is_unbounded() {
    for (dpi, huge) in [(1., false), (0.5, false), (1., true)] {
        let mut scene = Scene::new();
        scene.push_group(Group::default());
        scene.draw(Draw::Stroke {
            transform: if huge {
                Affine::scale(1000.)
            } else {
                Affine::IDENTITY
            },
            stroke: Stroke {
                miter_limit: 1.,
                ..Stroke::new(0.2)
            },
            brush: Brush::Solid(Color::WHITE),
            brush_transform: None,
            shape: Geometry::Rect(Rect::new(
                0.,
                0.,
                if huge { 1e307 } else { 20. },
                if huge { 1e307 } else { 20. },
            )),
            composite: Composite::default(),
        });
        scene.pop_group();
        let mut bridge = widget_bridge(RecordWidget::new(scene), Affine::IDENTITY);
        bridge.handle_window_event(WindowEvent::Rescale(dpi));
        let pool = pool();
        let mut builder = FrameBuild::new(
            bridge.record(FrameId(1)).unwrap(),
            empty_snapshot(&pool),
            FrameInfo {
                logical_size: [400. / dpi, 300. / dpi],
                physical_size: [400, 300],
            },
            limits(),
            &pool,
        )
        .unwrap();
        while !builder.step(step_budget()).unwrap().complete {}
        let frame = builder.finish().unwrap();
        let group = frame
            .commands()
            .iter()
            .find_map(|c| {
                if let FrameOp::PushGroup(g) = &c.op {
                    Some(g)
                } else {
                    None
                }
            })
            .unwrap();
        if huge {
            assert_eq!(group.ink_bounds, InkBounds::Unbounded);
        } else {
            let InkBounds::Rect(bounds) = group.ink_bounds else {
                panic!("known stroke bounds")
            };
            let aa = 1. / dpi;
            assert!(bounds.contains(Point::new(-aa, -aa)));
            assert!(bounds.contains(Point::new(20. + aa, 20. + aa)));
        }
    }
}
#[test]
fn nonfinite_record_has_a_localized_failure() {
    let mut scene = Scene::new();
    scene.draw(Draw::Fill {
        transform: Affine::new([f64::NAN, 0., 0., 1., 0., 0.]),
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::WHITE),
        brush_transform: None,
        shape: Geometry::Rect(Rect::new(0., 0., 20., 20.)),
        composite: Composite::default(),
    });
    let mut bridge = widget_bridge(RecordWidget::new(scene), Affine::IDENTITY);
    let pool = pool();
    let mut b = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        limits(),
        &pool,
    )
    .unwrap();
    let e = loop {
        if let Err(e) = b.step(step_budget()) {
            break e;
        }
    };
    assert_eq!(e.kind, BuildErrorKind::InvalidRecord);
    assert!(e.at.command > 0);
    assert_eq!(bridge.pending_count(), 0);
}
#[test]
#[ignore = "set REEL_R01_TEST_FONT to an installed font; no font source is vendored"]
fn actual_parley_label_glyphs_are_copied_without_reshaping() {
    use reel_ui::{
        app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
        dpi::PhysicalSize,
    };
    let data =
        std::fs::read(std::env::var("REEL_R01_TEST_FONT").expect("provide a real test font path"))
            .unwrap();
    let bytes = data.len();
    let font = Blob::from(data);
    let root = RenderRoot::new(
        reel_ui::widgets::Label::new("office ffi Reel 25").prepare(),
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(400, 300),
            scale_factor: 1.,
            default_font: Some(font.clone()),
        },
    );
    let pool = pool();
    let mut registry = ResourceRegistry::new(1, 4096).unwrap();
    let handle = registry
        .register(version(9), ResourceData::Font(font), ticket(&pool, bytes))
        .unwrap();
    let mut bridge = bridge(root);
    let source = bridge.record(FrameId(1)).unwrap();
    let mut expected = vec![];
    for layer in &source.plan().layers {
        if let reel_ui::app::VisualLayerKind::Scene(scene) = &layer.kind {
            for command in scene.commands() {
                if let record::Command::Draw(id) = command
                    && let Draw::GlyphRun(g) = scene.draw_op(*id)
                {
                    expected.push((
                        g.glyphs.clone(),
                        g.normalized_coords.clone(),
                        g.font_size,
                        g.font.index,
                    ));
                }
            }
        }
    }
    assert!(!expected.is_empty());
    let frame = build(source, registry.freeze(&[handle], &pool).unwrap(), &pool);
    let actual: Vec<_> = frame
        .commands()
        .iter()
        .filter_map(|c| {
            if let FrameOp::Draw(FrameDraw::GlyphRun(g)) = &c.op {
                assert_eq!(g.font, handle);
                Some((
                    g.glyphs.clone(),
                    g.normalized_coords.clone(),
                    g.font_size,
                    g.font_index,
                ))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(actual, expected);
    println!(
        "actual Parley Label: {} runs, {} positioned glyphs; font version pre-registered",
        actual.len(),
        actual.iter().map(|g| g.0.len()).sum::<usize>()
    );
}

#[test]
fn retained_subtree_is_balanced_independent_of_candidate_and_sibling_resources() {
    let pool = pool();
    let mut registry = ResourceRegistry::new(2, 8192).unwrap();
    let old_image = fixture::image_data([255, 0, 0, 255]);
    let sibling_image = fixture::image_data([0, 255, 0, 255]);
    let handles = [old_image.clone(), sibling_image.clone()]
        .into_iter()
        .enumerate()
        .map(|(index, image)| {
            registry
                .register(
                    version(index as u8),
                    ResourceData::Image {
                        image,
                        encoding: ColorEncoding::Srgb,
                    },
                    ticket(&pool, 4),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    let weak = registry.acquire(handles[1]).unwrap().downgrade();
    let image_scene = |image| {
        let mut scene = Scene::new();
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Image(peniko::ImageBrush::new(image)),
            brush_transform: None,
            shape: Geometry::Rect(Rect::new(0., 0., 20., 20.)),
            composite: Default::default(),
        });
        scene
    };
    let mut parent = RecordWidget::new(image_scene(sibling_image));
    let mut child = RecordWidget::new(image_scene(old_image));
    child.opacity = 0.5;
    child.blend = BlendMode::Subtract;
    let child_id = parent
        .inner
        .add(child.prepare(), Point::new(25., 30.), Size::new(70., 60.));
    let mut bridge = widget_bridge(parent, Affine::scale(1.5));
    let source = bridge.record(FrameId(1)).unwrap();
    let unsupported = source.scene().unwrap_err();
    assert_eq!(unsupported.kind, BuildErrorKind::UnsupportedRecord);
    assert_eq!(unsupported.at.owner, child_id);
    let frame = build(source, registry.freeze(&handles, &pool).unwrap(), &pool);
    let mut retaining = RetainBuild::new(&frame, Some(child_id), limits(), &pool).unwrap();
    for handle in &handles {
        registry.retire(*handle);
    }
    drop(frame);
    assert_eq!(
        bridge.pending_count(),
        0,
        "retention must not retain an input candidate guard"
    );
    let zero = retaining
        .step(StepBudget {
            max_commands: 0,
            max_units: 0,
            max_bytes: 0,
        })
        .unwrap();
    assert_eq!(zero.commands, 0);
    assert!(!zero.complete);
    loop {
        let budget = step_budget();
        let progress = retaining.step(budget).unwrap();
        assert!(
            progress.commands <= budget.max_commands
                && progress.units <= budget.max_units
                && progress.bytes <= budget.max_bytes
        );
        if progress.complete {
            break;
        }
    }
    let retained = retaining.finish().unwrap();
    assert_eq!(retained.drawing().resources().leases().len(), 1);
    assert_eq!(
        retained.drawing().resources().leases()[0].handle(),
        handles[0]
    );
    assert!(retained.drawing().resources().leases()[0].is_retired());
    assert!(
        weak.is_released(),
        "unrelated sibling resource must leave with the candidate"
    );
    let commands = retained.drawing().commands();
    assert!(matches!(
        commands.first().unwrap().op,
        FrameOp::PushGroup(_)
    ));
    assert!(matches!(commands.last().unwrap().op, FrameOp::PopGroup));
    assert_eq!(
        commands
            .iter()
            .filter(|c| matches!(c.op, FrameOp::Draw(_)))
            .count(),
        1
    );
    assert_eq!(retained.to_parent(), Affine::scale(1. / 1.5));
    let saved = retained.snapshot(|_| None).unwrap();
    let mut restoring = saved.restore_begin(limits(), &pool).unwrap();
    while !restoring.step(step_budget()).unwrap().build.complete {}
    let restored = restoring.finish().unwrap();
    let FrameOp::PushGroup(group) = &restored.drawing().commands()[0].op else {
        panic!("retained subtree must keep its group");
    };
    assert_eq!(group.composite.blend, BlendMode::Subtract);
    assert_eq!(group.composite.alpha, 0.5);
}

#[test]
fn retained_reusable_mask_copies_all_commands_with_tiny_steps() {
    let pool = pool();
    let mut scene = Scene::new();
    let mut mask_scene = Scene::new();
    for _ in 0..11 {
        mask_scene.append_transformed(&fill(Color::WHITE), Affine::IDENTITY);
    }
    let mask = scene.define_mask(Mask::new(MaskMode::Alpha, mask_scene));
    scene.push_group(Group {
        mask: Some(AppliedMask {
            mask,
            transform: Affine::IDENTITY,
        }),
        ..Default::default()
    });
    scene.append_transformed(&fill(Color::WHITE), Affine::IDENTITY);
    scene.pop_group();
    let mut bridge = widget_bridge(RecordWidget::new(scene), Affine::IDENTITY);
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    let mut retaining = RetainBuild::new(&frame, None, limits(), &pool).unwrap();
    let mut budget = StepBudget {
        max_commands: 1,
        max_units: 1,
        max_bytes: 512,
    };
    let mut steps = 0;
    let mut largest_minimum = 0;
    loop {
        let progress = retaining.step(budget).unwrap();
        if progress.complete {
            break;
        }
        assert!(
            progress.commands <= budget.max_commands
                && progress.bytes <= budget.max_bytes
                && progress.units <= budget.max_units
        );
        budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
        budget.max_units = budget.max_units.max(progress.minimum_units);
        largest_minimum = largest_minimum.max(progress.minimum_bytes);
        steps += 1;
        assert!(steps < 100);
    }
    let retained = retaining.finish().unwrap();
    assert_eq!(retained.drawing().masks().len(), 1);
    assert_eq!(retained.drawing().masks()[&0].commands.len(), 11);
    assert!(
        largest_minimum > 512,
        "mask reference storage must request its allocation before proceeding"
    );
    println!("retain mask: {steps} slices, largest requested allocation {largest_minimum} bytes");
}

#[test]
fn retained_empty_siblings_at_one_command_gap_keep_separate_transitions() {
    let pool = pool();
    let mut parent = RecordWidget::new(fill(Color::BLACK));
    let first = parent.inner.add(
        RecordWidget::new(fill(Color::WHITE)).prepare(),
        Point::new(10., 10.),
        Size::new(30., 30.),
    );
    let second = parent.inner.add(
        RecordWidget::new(fill(Color::BLACK)).prepare(),
        Point::new(60., 10.),
        Size::new(30., 30.),
    );
    let parent = parent.prepare();
    let parent_id = parent.id();
    let mut bridge = r01_fixture::bridge(fixture::root(parent).0);
    let old = build(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    let retain = |frame: &RenderFrame, root| {
        let mut build = RetainBuild::new(frame, root, limits(), &pool).unwrap();
        while !build.step(step_budget()).unwrap().complete {}
        build.finish().unwrap()
    };
    let previous = [retain(&old, Some(first)), retain(&old, Some(second))];
    drop(old);
    bridge.edit_widget(parent_id, |mut node| {
        let mut node = node.downcast::<RecordWidget>();
        for child in &mut node.widget.inner.children {
            node.ctx.set_stashed(&mut child.0, true);
        }
    });
    let mut builder = FrameBuild::new(
        bridge.record(FrameId(2)).unwrap(),
        empty_snapshot(&pool),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        limits(),
        &pool,
    )
    .unwrap();
    builder
        .set_transitions(vec![
            FrameTransition {
                root: Some(second),
                previous: previous[1].clone(),
                progress: 0.75,
                mask: None,
            },
            FrameTransition {
                root: Some(first),
                previous: previous[0].clone(),
                progress: 0.25,
                mask: None,
            },
        ])
        .unwrap();
    while !builder.step(step_budget()).unwrap().complete {}
    let frame = builder.finish().unwrap();
    assert_eq!(
        frame.drawing().transitions()[0].commands,
        frame.drawing().transitions()[1].commands
    );
    for (root, expected) in [(first, 0.25), (second, 0.75)] {
        let picture = retain(&frame, Some(root));
        assert!(picture.drawing().commands().is_empty());
        assert_eq!(picture.drawing().transitions().len(), 1);
        assert_eq!(picture.drawing().transitions()[0].progress, expected);
    }
}

#[test]
fn retained_large_command_waits_for_payload_budget_then_owns_only_selected_copy() {
    let pool = pool();
    let mut path = BezPath::with_capacity(8193);
    path.move_to((0., 0.));
    for i in 0..8192 {
        path.line_to(((i % 100) as f64, (i / 100) as f64));
    }
    let mut scene = Scene::new();
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::WHITE),
        brush_transform: None,
        shape: Geometry::Path(path),
        composite: Default::default(),
    });
    let mut parent = RecordWidget::new(fill(Color::BLACK));
    let child = parent.inner.add(
        RecordWidget::new(scene).prepare(),
        Point::new(10., 10.),
        Size::new(100., 100.),
    );
    let mut bridge = widget_bridge(parent, Affine::IDENTITY);
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    let path = frame
        .commands()
        .iter()
        .find_map(|command| match &command.op {
            FrameOp::Draw(FrameDraw::Fill {
                shape: Geometry::Path(path),
                ..
            }) => Some(path),
            _ => None,
        })
        .unwrap();
    let source_pointer = path.elements().as_ptr();
    let expected = path.elements().to_vec();
    let initial = pool.usage().bytes;
    let small = StepBudget {
        max_commands: 8,
        max_units: 128,
        max_bytes: 1024,
    };
    let mut retaining = RetainBuild::new(&frame, Some(child), limits(), &pool).unwrap();
    let blocked = retaining.step(small).unwrap();
    assert!(blocked.minimum_bytes >= (8193 * size_of::<reel_ui::kurbo::PathEl>()) as u64);
    assert!(blocked.minimum_units > small.max_units);
    let still_blocked = retaining.step(small).unwrap();
    assert_eq!(still_blocked.commands, 0);
    assert_eq!(still_blocked.bytes, 0);
    drop(retaining);
    assert_eq!(
        pool.usage().bytes,
        initial,
        "cancellation releases the partial copy"
    );

    let mut retaining = RetainBuild::new(&frame, Some(child), limits(), &pool).unwrap();
    let mut budget = small;
    loop {
        let progress = retaining.step(budget).unwrap();
        assert!(progress.units <= budget.max_units && progress.bytes <= budget.max_bytes);
        if progress.complete {
            break;
        }
        budget.max_units = budget.max_units.max(progress.minimum_units);
        budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
    }
    let retained = retaining.finish().unwrap();
    assert_eq!(retained.drawing().commands().len(), 3);
    let FrameOp::Draw(FrameDraw::Fill {
        shape: Geometry::Path(path),
        ..
    }) = &retained.drawing().commands()[1].op
    else {
        panic!("retained selected path")
    };
    assert_ne!(path.elements().as_ptr(), source_pointer);
    drop(frame);
    drop(bridge);
    assert_eq!(path.elements(), expected);
    drop(retained);
    assert_eq!(pool.usage().bytes, 0);
}
