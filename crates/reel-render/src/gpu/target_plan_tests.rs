//! Storage admission contracts use geometry fixtures without GPU readback.
use super::*;
#[allow(dead_code, reason = "shared UI fixture")]
#[path = "../../../reel-ui/tests/support/fixture.rs"]
mod fixture;
#[allow(dead_code, reason = "shared immutable frame fixture")]
#[path = "../../../reel-ui/tests/support/r01_fixture.rs"]
mod r01_fixture;
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
fn finite_group_admission_matches_clipped_physical_storage_for_every_sample_tier() {
    let size = [960, 544];
    let pool = r01_fixture::pool();
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
                ..Default::default()
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
                |_| Ok(()),
            )
            .unwrap();
            assert_eq!(
                pixels,
                u64::from(size[0]) * u64::from(size[1]) + source_pixels
            );
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
        |_| Ok(()),
    )
    .unwrap();
    assert!(schedule.is_empty());
    assert_eq!(pixels, 960 * 544);
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
