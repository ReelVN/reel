use super::*;
use crate::{
    common::{glyph_plan, image_plan, retained},
    test_support::r01_fixture as f,
};
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
};
use reel_ui::{
    core::Widget,
    imaging::{
        Composite,
        record::{Clip, Draw, Group, Scene},
    },
    peniko::{Brush, Color, ImageAlphaType, ImageFormat},
    resources::{ColorEncoding, ResourceRegistry},
};

fn fill(scene: &mut Scene, rect: Rect, transform: Affine, composite: Composite) {
    scene.draw(Draw::Fill {
        transform,
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::WHITE),
        brush_transform: None,
        shape: Geometry::Rect(rect),
        composite,
    });
}
fn frame(scene: Scene) -> RenderFrame {
    frame_with_logical(scene, [400., 300.])
}
fn frame_with_logical(scene: Scene, logical_size: [f64; 2]) -> RenderFrame {
    let pool = f::pool();
    let mut widget = f::RecordWidget::new(scene);
    widget.clip = false;
    let mut bridge = f::widget_bridge(widget, Affine::IDENTITY);
    let mut build = reel_ui::frame::FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        f::empty_snapshot(&pool),
        reel_ui::frame::FrameInfo {
            logical_size,
            physical_size: [400, 300],
        },
        f::limits(),
        &pool,
    )
    .unwrap();
    let mut budget = f::step_budget();
    loop {
        let progress = build.step(budget).unwrap();
        if progress.complete {
            return build.finish().unwrap();
        }
        budget.max_units = budget.max_units.max(progress.minimum_units);
        budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
    }
}
fn selection(frame: &RenderFrame) -> retained::Plan {
    let budgets = Budgets::new(Default::default());
    retained::Plan::collect(
        std::iter::once(frame),
        [400, 300],
        false,
        &budgets,
        &budgets,
        32 << 20,
        |_| None,
        |_| Ok(()),
    )
    .unwrap()
}
fn draws(frame: &RenderFrame, plan: &retained::Plan) -> Vec<bool> {
    frame
        .commands()
        .iter()
        .enumerate()
        .filter(|(_, command)| matches!(command.op, FrameOp::Draw(_)))
        .map(|(index, _)| plan.selected(frame.drawing(), index))
        .collect()
}
fn rect(x: u32, y: u32, width: u32, height: u32) -> Scissor {
    Scissor {
        x,
        y,
        width,
        height,
    }
}

#[test]
fn bounded_union_proves_adjacent_rectangles_and_retains_holes() {
    let mut coverage = Coverage::default();
    coverage.insert(rect(0, 0, 40, 100));
    coverage.insert(rect(60, 0, 40, 100));
    assert!(!coverage.covers(rect(0, 0, 100, 100)));
    coverage.insert(rect(40, 0, 20, 100));
    assert!(coverage.covers(rect(0, 0, 100, 100)));
    for index in 0..100 {
        coverage.insert(rect(index * 2, 200, 1, 1));
    }
    assert!(!coverage.covers(rect(0, 200, 200, 1)));
}

#[test]
fn recorded_union_eliminates_only_fully_covered_draws() {
    let mut scene = Scene::new();
    for rect in [
        Rect::new(0., 0., 100., 100.),
        Rect::new(0., 0., 40., 100.),
        Rect::new(40., 0., 100., 100.),
    ] {
        fill(&mut scene, rect, Affine::IDENTITY, Default::default());
    }
    let frame = frame(scene);
    assert_eq!(draws(&frame, &selection(&frame)), [false, true, true]);
}

#[test]
fn crossfade_streams_use_replays_first_frame_grid() {
    let first = frame(Scene::new());
    let mut scene = Scene::new();
    for _ in 0..2 {
        fill(
            &mut scene,
            Rect::new(1., 1., 101., 101.),
            Affine::IDENTITY,
            Default::default(),
        );
    }
    let second = frame_with_logical(scene, [800., 600.]);
    let budgets = Budgets::new(Default::default());
    let plan = retained::Plan::collect(
        [&first, &second].into_iter(),
        [400, 300],
        false,
        &budgets,
        &budgets,
        32 << 20,
        |_| None,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(draws(&second, &plan), [false, true]);
    // At the second frame's own half-density grid the fractional edge pixels
    // have only partial coverage; that is not the grid used by crossfade replay.
    assert_eq!(draws(&second, &selection(&second)), [true, true]);
}

#[test]
fn fractional_edges_keep_border_pixels_and_integer_flips_share_the_grid() {
    for (transform, expected) in [
        (Affine::IDENTITY, false),
        (
            Affine::translate((100., 0.)) * Affine::scale_non_uniform(-1., 1.),
            false,
        ),
        (Affine::new([0., 1., -1., 0., 100., 0.]), false),
        (Affine::translate((0.25, 0.25)), true),
        (Affine::rotate(0.1), true),
    ] {
        let mut scene = Scene::new();
        fill(
            &mut scene,
            Rect::new(0., 0., 100., 100.),
            Affine::IDENTITY,
            Default::default(),
        );
        fill(
            &mut scene,
            Rect::new(0., 0., 100., 100.),
            transform,
            Default::default(),
        );
        let frame = frame(scene);
        assert_eq!(
            draws(&frame, &selection(&frame))[0],
            expected,
            "{transform:?}"
        );
    }
    assert_eq!(
        pixels(
            Rect::new(0.25, 0.25, 100.25, 100.25),
            [400, 300],
            true,
            true
        ),
        Some(rect(1, 1, 99, 99))
    );
}

#[test]
fn bounds_follow_local_float32_vertices_before_large_coordinate_cancellation() {
    let mut scene = Scene::new();
    fill(
        &mut scene,
        Rect::new(1_000_000_001., 0., 1_000_000_101., 100.),
        Affine::translate((-1_000_000_000., 0.)),
        Default::default(),
    );
    fill(
        &mut scene,
        Rect::new(1., 0., 101., 100.),
        Affine::IDENTITY,
        Default::default(),
    );
    let frame = frame(scene);
    assert_eq!(draws(&frame, &selection(&frame)), [true, true]);
    let draw = frame
        .commands()
        .iter()
        .find_map(|c| {
            if let FrameOp::Draw(draw) = &c.op {
                Some(draw)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        draw_rect(draw, Affine::IDENTITY),
        Some((Rect::new(0., 0., 128., 100.), true))
    );
}

#[test]
fn inner_and_outer_bounds_keep_all_single_and_four_sample_positions() {
    for x in [0., 0.125, 0.5, 0.875, 1.] {
        for y in [0., 0.125, 0.5, 0.875, 1.] {
            let bounds = Rect::new(x, y, 6. + y, 6. + x);
            let inner = pixels(bounds, [8, 8], true, true).unwrap();
            let outer = pixels(bounds, [8, 8], false, true).unwrap();
            for py in 0..8 {
                for px in 0..8 {
                    let pixel = rect(px, py, 1, 1);
                    let mut any = false;
                    for [sx, sy] in [
                        [0.5, 0.5],
                        [0.375, 0.125],
                        [0.875, 0.375],
                        [0.125, 0.625],
                        [0.625, 0.875],
                    ] {
                        let inside = bounds.contains((f64::from(px) + sx, f64::from(py) + sy));
                        if inner.intersect(pixel) == pixel {
                            assert!(inside);
                        }
                        any |= inside;
                    }
                    if any {
                        assert_eq!(outer.intersect(pixel), pixel);
                    }
                }
            }
        }
    }
}

#[test]
fn scissor_is_shared_but_background_and_isolation_end_coverage() {
    let mut scene = Scene::new();
    fill(
        &mut scene,
        Rect::new(10., 10., 90., 90.),
        Affine::IDENTITY,
        Default::default(),
    );
    scene.push_clip(Clip::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        shape: Geometry::Rect(Rect::new(10., 10., 90., 90.)),
    });
    fill(
        &mut scene,
        Rect::new(0., 0., 100., 100.),
        Affine::IDENTITY,
        Default::default(),
    );
    scene.pop_clip();
    let clipped = frame(scene);
    assert_eq!(draws(&clipped, &selection(&clipped)), [false, true]);
    let mut scene = Scene::new();
    fill(
        &mut scene,
        Rect::new(10., 10., 90., 90.),
        Affine::IDENTITY,
        Default::default(),
    );
    scene.push_clip(Clip::Fill {
        transform: Affine::translate((0.25, 0.25)),
        fill_rule: Default::default(),
        shape: Geometry::Rect(Rect::new(0., 0., 100., 100.)),
    });
    fill(
        &mut scene,
        Rect::new(0., 0., 100., 100.),
        Affine::IDENTITY,
        Default::default(),
    );
    scene.pop_clip();
    fill(
        &mut scene,
        Rect::new(0., 0., 100., 100.),
        Affine::IDENTITY,
        Default::default(),
    );
    let unknown_clip = frame(scene);
    assert_eq!(
        draws(&unknown_clip, &selection(&unknown_clip)),
        [true, true, true]
    );
    for alpha in [0.5, 1.] {
        let mut scene = Scene::new();
        fill(
            &mut scene,
            Rect::new(0., 0., 100., 100.),
            Affine::IDENTITY,
            Default::default(),
        );
        scene.push_group(Group {
            composite: Composite {
                alpha,
                blend: reel_ui::peniko::Mix::Multiply.into(),
            },
            ..Default::default()
        });
        fill(
            &mut scene,
            Rect::new(0., 0., 100., 100.),
            Affine::IDENTITY,
            Default::default(),
        );
        scene.pop_group();
        fill(
            &mut scene,
            Rect::new(0., 0., 100., 100.),
            Affine::IDENTITY,
            Default::default(),
        );
        let frame = frame(scene);
        assert_eq!(draws(&frame, &selection(&frame)), [true, true, true]);
    }
}

#[test]
fn mixed_parent_keeps_independent_segments_on_both_sides_of_a_backdrop_reader() {
    let mut scene = Scene::new();
    for _ in 0..2 {
        fill(
            &mut scene,
            Rect::new(0., 0., 100., 100.),
            Affine::IDENTITY,
            Default::default(),
        );
    }
    scene.push_group(Group {
        composite: Composite {
            alpha: 0.5,
            blend: reel_ui::peniko::Mix::Multiply.into(),
        },
        ..Default::default()
    });
    fill(
        &mut scene,
        Rect::new(0., 0., 100., 100.),
        Affine::IDENTITY,
        Default::default(),
    );
    scene.pop_group();
    for _ in 0..2 {
        fill(
            &mut scene,
            Rect::new(0., 0., 100., 100.),
            Affine::IDENTITY,
            Default::default(),
        );
    }
    let frame = frame(scene);
    assert_eq!(
        draws(&frame, &selection(&frame)),
        [false, true, true, false, true]
    );
}

fn image_frame(alpha: u8, clipped: bool) -> RenderFrame {
    image_frame_in_group(alpha, clipped, 1.)
}
fn image_frame_in_group(alpha: u8, clipped: bool, opacity: f32) -> RenderFrame {
    let pool = f::pool();
    let mut registry = ResourceRegistry::new(3, 1 << 20).unwrap();
    let mut widget = f::RecordWidget::new(Scene::new());
    widget.clip = false;
    widget.opacity = opacity;
    let mut handles = Vec::new();
    for key in [1, 2] {
        let mut bytes = vec![255; 4 * 4 * 4];
        if key == 2 {
            bytes[(2 * 4 + 2) * 4 + 3] = alpha;
        }
        let handle = registry
            .register_image(
                ResourceVersion {
                    key: AssetKey(format!("occlusion-{key}")),
                    version: ContentVersion([key; 32]),
                },
                OwnedBytes::new(bytes, pool.reserve(64).unwrap()).unwrap(),
                [4, 4],
                ImageFormat::Rgba8,
                ImageAlphaType::Alpha,
                ColorEncoding::Srgb,
            )
            .unwrap();
        handles.push(handle);
        let mut child = f::RecordWidget::new(Scene::new());
        child.clip = clipped;
        child.resource_draws.push((
            registry.acquire(handle).unwrap(),
            Rect::new(0., 0., 100., 100.),
            Affine::IDENTITY,
            Default::default(),
        ));
        widget.inner.add(
            child.prepare(),
            reel_ui::kurbo::Point::ZERO,
            reel_ui::kurbo::Size::new(100., 100.),
        );
    }
    let mut bridge = f::widget_bridge(widget, Affine::IDENTITY);
    f::build(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&handles, &pool).unwrap(),
        &pool,
    )
}

#[test]
fn recorded_rgba_widgets_remove_upload_demand_and_eager_batches_stop_before_hidden_images() {
    for clipped in [false, true] {
        let frame = image_frame(255, clipped);
        let selected = selection(&frame);
        assert_eq!(draws(&frame, &selected), [false, true], "clipped={clipped}");
        let complete = retained::Plan::default();
        let required = |plan| {
            image_plan::collect(std::iter::once(&frame), 4096, false, plan, |_| Ok(()))
                .unwrap()
                .images
                .len()
        };
        assert_eq!((required(&complete), required(&selected)), (2, 1));
        let hidden = frame
            .commands()
            .iter()
            .enumerate()
            .find(|(_, c)| matches!(c.op, FrameOp::Draw(_)))
            .unwrap()
            .0;
        assert_eq!(
            selected.draw_batch_end(frame.drawing(), Some(0), 0, frame.commands().len()),
            hidden
        );
        assert_eq!(
            selected.draw_batch_end(frame.drawing(), Some(0), hidden + 1, frame.commands().len()),
            frame.commands().len()
        );
    }
    let transparent = image_frame(254, false);
    assert_eq!(draws(&transparent, &selection(&transparent)), [true, true]);
    let isolated = image_frame_in_group(255, true, 0.37);
    let selected = selection(&isolated);
    assert_eq!(draws(&isolated, &selected), [false, true]);
    assert_eq!(
        image_plan::collect(std::iter::once(&isolated), 4096, false, &selected, |_| Ok(
            ()
        ))
        .unwrap()
        .images
        .len(),
        1
    );
}

#[test]
fn capture_and_zero_metadata_budget_preserve_all_draws_without_charges() {
    let frame = image_frame(255, false);
    let limits = super::super::GpuLimits {
        transient_bytes: 0,
        ..Default::default()
    };
    let budgets = Budgets::new(limits);
    let plan = Plan::collect(
        std::iter::once(&frame),
        [400, 300],
        false,
        &budgets,
        &budgets,
        0,
    )
    .unwrap();
    assert!(plan.rows.is_empty());
    assert_eq!(budgets.transient.usage().bytes, 0);
    let budgets = Budgets::new(Default::default());
    let plan = Plan::collect(
        std::iter::once(&frame),
        [400, 300],
        true,
        &budgets,
        &budgets,
        32 << 20,
    )
    .unwrap();
    assert!(plan.rows.is_empty());
    assert_eq!(budgets.transient.usage().bytes, 0);
}

#[derive(Default)]
struct Glyphs(usize);
impl glyph_plan::Prepare for Glyphs {
    fn reserve(&mut self, _: u64) -> Result<(), GpuError> {
        Ok(())
    }
    fn finish(&mut self) -> Result<(), GpuError> {
        Ok(())
    }
    fn run(
        &mut self,
        _: &FrameDrawing,
        run: &reel_ui::frame::FrameGlyphRun,
        _: f64,
        _: reel_ui::frame::RecordLocation,
    ) -> Result<reel_ui::frame::InkBounds, GpuError> {
        self.0 += run.glyphs.len();
        Ok(reel_ui::frame::InkBounds::Unbounded)
    }
}
#[test]
fn opaque_foreground_omits_real_recorded_text_before_glyph_preparation() {
    use reel_ui::{
        app::{RenderRoot, RenderRootOptions, VisualLayerKind, WindowSizePolicy},
        core::{DefaultProperties, NewWidget},
        dpi::PhysicalSize,
        imaging::record::Command,
        peniko::Blob,
        resources::ResourceData,
        widgets::Label,
    };
    let pool = f::pool();
    let mut registry = ResourceRegistry::new(1, 1 << 20).unwrap();
    let font = Blob::from(
        include_bytes!(
            "../../../../examples/author_arcade/resources/fonts/AtkinsonHyperlegible-Regular.ttf"
        )
        .to_vec(),
    );
    let handle = registry
        .register(
            ResourceVersion {
                key: AssetKey("occluded-text".into()),
                version: ContentVersion([7; 32]),
            },
            ResourceData::Font(font.clone()),
            pool.reserve(font.len() as u64).unwrap(),
        )
        .unwrap();
    let mut root = RenderRoot::new(
        NewWidget::new(Label::new("covered text")),
        |_| {},
        RenderRootOptions {
            default_properties: std::sync::Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(400, 300),
            scale_factor: 1.,
            default_font: Some(font),
        },
    );
    let (layers, _) = root.redraw();
    let mut scene = Scene::new();
    for layer in &layers.layers {
        if let VisualLayerKind::Scene(source) = &layer.kind {
            for command in source.commands() {
                if let Command::Draw(id) = command
                    && let Draw::GlyphRun(run) = source.draw_op(*id)
                {
                    scene.draw(Draw::GlyphRun(run.clone()));
                }
            }
        }
    }
    fill(
        &mut scene,
        Rect::new(0., 0., 400., 300.),
        Affine::IDENTITY,
        Default::default(),
    );
    let mut widget = f::RecordWidget::new(scene);
    widget.clip = false;
    let mut bridge = f::widget_bridge(widget, Affine::IDENTITY);
    let frame = f::build(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[handle], &pool).unwrap(),
        &pool,
    );
    let selected = selection(&frame);
    let mut before = Glyphs::default();
    let mut after = Glyphs::default();
    glyph_plan::Plan::collect(
        std::iter::once(&frame),
        Affine::IDENTITY,
        4096,
        false,
        &retained::Plan::default(),
        &mut before,
    )
    .unwrap();
    glyph_plan::Plan::collect(
        std::iter::once(&frame),
        Affine::IDENTITY,
        4096,
        false,
        &selected,
        &mut after,
    )
    .unwrap();
    assert!(before.0 > 0);
    assert_eq!(after.0, 0);
    assert_eq!(draws(&frame, &selected).last(), Some(&true));
}

#[test]
#[ignore = "release CPU-only shared occlusion planning cost; no GPU or readback"]
fn occlusion_planning_cost() {
    use std::{hint::black_box, time::Instant};
    for overlap in [false, true] {
        let mut scene = Scene::new();
        for index in 0..512 {
            let rect = if overlap {
                Rect::new(0., 0., 400., 300.)
            } else {
                let x = (index % 32) as f64 * 10.;
                let y = (index / 32) as f64 * 10.;
                Rect::new(x, y, x + 8., y + 8.)
            };
            fill(&mut scene, rect, Affine::IDENTITY, Default::default());
        }
        let frame = frame(scene);
        let budgets = Budgets::new(Default::default());
        let local = Budgets::new(Default::default());
        let collect = || {
            Plan::collect(
                std::iter::once(&frame),
                [400, 300],
                false,
                &budgets,
                &local,
                32 << 20,
            )
            .unwrap()
        };
        let plan = collect();
        let selected = frame
            .commands()
            .iter()
            .enumerate()
            .filter(|(index, c)| {
                matches!(c.op, FrameOp::Draw(_)) && plan.selected(frame.drawing(), *index)
            })
            .count();
        assert_eq!(selected, if overlap { 1 } else { 512 });
        let bytes = budgets.transient.usage().bytes;
        drop(plan);
        let start = Instant::now();
        for _ in 0..1000 {
            black_box(collect());
        }
        eprintln!(
            "occlusion overlap={overlap} draws=512 selected={selected} retained_metadata_bytes={bytes} plan_us={:.2}",
            start.elapsed().as_secs_f64() * 1000.
        );
        assert_eq!(budgets.transient.usage().bytes, 0);
    }
}
