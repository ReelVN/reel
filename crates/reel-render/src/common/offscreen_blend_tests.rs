use super::*;
use crate::test_support::r01_fixture as fixture;
use reel_core::identity::FrameId;
use reel_ui::{
    frame::{FrameBuild, FrameInfo},
    imaging::{
        Composite,
        record::{Draw, Geometry, Group, Scene},
    },
    kurbo::Rect,
    peniko::{Brush, Color, Mix},
};

fn siblings(full: bool, wrapped: bool, clipped: bool) -> RenderFrame {
    let mut scene = Scene::new();
    if wrapped {
        scene.push_group(Group::default());
    }
    for index in 0..2 {
        scene.push_group(Group {
            composite: Composite {
                alpha: 0.75,
                blend: Mix::Multiply.into(),
            },
            ..Default::default()
        });
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Solid(Color::from_rgba8(100 + index * 30, 120, 180, 160)),
            brush_transform: None,
            shape: Geometry::Rect(if full {
                Rect::new(0., 0., 960., 544.)
            } else {
                Rect::new(20., 20., 100., 80.)
            }),
            composite: Default::default(),
        });
        scene.pop_group();
    }
    if wrapped {
        scene.pop_group();
    }
    let pool = fixture::pool();
    let mut widget = fixture::RecordWidget::new(scene);
    widget.clip = clipped;
    widget.inner.clip = Rect::new(0., 0., 960., 544.);
    let mut bridge = fixture::widget_bridge(widget, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        fixture::empty_snapshot(&pool),
        FrameInfo {
            logical_size: [960., 544.],
            physical_size: [960, 544],
        },
        fixture::limits(),
        &pool,
    )
    .unwrap();
    while !build.step(fixture::step_budget()).unwrap().complete {}
    build.finish().unwrap()
}

#[test]
fn rejected_frozen_input_owners_release_their_budget_before_the_next_attempt() {
    use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
    let frame = siblings(true, false, false);
    let pool = BudgetPool::new(
        BudgetCategory::Temporary,
        QueueLimits {
            max_requests: 1,
            max_bytes: 128,
        },
    );
    let mut calls = 0;
    let result = Plan::choose_with_groups(
        &frame,
        None,
        [960, 544],
        false,
        1,
        4096,
        &glyph_plan::Plan::default(),
        0,
        &|size| Ok(u64::from(size[0]) * u64::from(size[1]) * 4),
        None,
        super::super::target_plan::MaskExecution::Inline,
        super::super::blend::DestinationAccess::Sampled,
        &Default::default(),
        None,
        None,
        |_, bytes| {
            assert_eq!(pool.usage().bytes, 0);
            calls += 1;
            Ok((bytes, pool.reserve(128).unwrap()))
        },
        |_| Ok(()),
    )
    .unwrap();
    assert!(
        calls >= 2,
        "a rejected frozen attempt precedes the final fallback"
    );
    assert!(
        result.1 > 0,
        "zero device budget leaves admission to the caller"
    );
    assert_eq!(
        pool.usage().bytes,
        128,
        "only the returned input owner remains"
    );
    drop(result);
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
fn current_pixel_blends_defer_original_sources_with_their_full_lifetimes() {
    for (full, wrapped, clipped) in [
        (false, false, false),
        (true, false, false),
        (true, true, false),
        (false, false, true),
        (true, true, true),
    ] {
        let frame = siblings(full, wrapped, clipped);
        let choose = |destination, limit| {
            Plan::choose_with_groups(
                &frame,
                None,
                [960, 544],
                false,
                1,
                4096,
                &glyph_plan::Plan::default(),
                limit,
                &|s| Ok(u64::from(s[0]) * u64::from(s[1]) * 4),
                None,
                super::super::target_plan::MaskExecution::Inline,
                destination,
                &Default::default(),
                None,
                None,
                |_, bytes| Ok((bytes, ())),
                |_| Ok(()),
            )
            .unwrap()
        };
        let (ordinary, ordinary_bytes, _, ()) =
            choose(super::super::blend::DestinationAccess::Sampled, u64::MAX);
        assert!(ordinary.is_empty());
        let access = super::super::blend::DestinationAccess::CurrentPixelUnorm8;
        let (plan, bytes, _, ()) = choose(access, u64::MAX);
        let entries: Vec<_> = (0..frame.commands().len())
            .filter_map(|i| plan.entry(i))
            .collect();
        assert_eq!(
            entries.len(),
            2,
            "full={full} wrapped={wrapped} clipped={clipped}"
        );
        assert!(entries.iter().all(|e| e.view.is_none() == full));
        assert!(entries[0].first && entries[1].last);
        let mut cursor = plan.parent_cursor(entries[0].push);
        assert!(
            matches!(plan.next_parent(frame.commands(), &mut cursor), Some(ParentStep::Sources(range)) if range == (0..2))
        );
        assert!(plan.next_parent(frame.commands(), &mut cursor).is_none());
        if full {
            assert_eq!(bytes, 960 * 544 * 4 * (3 + u64::from(wrapped)));
            let (_, tight_bytes, _, ()) = choose(access, ordinary_bytes.min(bytes - 1));
            assert!(tight_bytes < bytes);
        }
    }
}
