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
            let pixels =
                target_pixels(&frame, None, size, false, samples, 4096, &plan, |_| Ok(())).unwrap();
            assert_eq!(
                pixels,
                u64::from(size[0]) * u64::from(size[1]) + source_pixels
            );
        }
    }
}
