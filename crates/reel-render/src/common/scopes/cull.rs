//! Reject invisible completed groups before entering their effect-local grids.
//! GPU User's Guide, chapter 14 (p169): cull whole invisible objects on the CPU.
use super::*;
use crate::common::scissor::Scissor;
use reel_ui::kurbo::Affine;

/// Return the complete balanced body length, including its closing group.
/// Bounds describe the group's output, so a visible effect still receives its
/// complete source even when it samples beyond the parent's clipped viewport.
/// Retained and capture boundaries keep their independent materialization.
pub(crate) fn invisible_group(
    group: &FrameGroup,
    body: &[FrameCommand],
    transform: Affine,
    size: [u32; 2],
    scissor: Scissor,
    boundaries: bool,
) -> Option<usize> {
    let bounds = Scissor::group_bounds(group, transform, size, boundaries)?;
    if !scissor.intersect(bounds).empty() {
        return None;
    }
    let mut depth = 1;
    for (index, command) in body.iter().enumerate() {
        match command.op {
            FrameOp::PushGroup(_) => depth += 1,
            FrameOp::PopGroup => {
                depth -= 1;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_ui::{frame::InkBounds, kurbo::Rect};

    fn group(ink_bounds: InkBounds) -> FrameGroup {
        FrameGroup {
            image_mask: None,
            effect: None,
            mask_effect: None,
            clip: None,
            mask: None,
            filters: Vec::new(),
            composite: Default::default(),
            invert: 0.,
            owner: reel_ui::core::WidgetPod::new(reel_ui::widgets::Label::new("")).id(),
            effect_to_target: Affine::translate((-2000., 0.)),
            ink_bounds,
        }
    }

    #[test]
    fn output_bounds_skip_balanced_children_and_preserve_visible_sampling_domains() {
        let mut scene = reel_ui::imaging::record::Scene::new();
        scene.push_group(reel_ui::imaging::record::Group::default());
        scene.pop_group();
        // Use a validated compiled stream so context commands are included in
        // the balanced range exactly as both replayers receive them.
        let pool = crate::test_support::r01_fixture::pool();
        let mut widget = crate::test_support::r01_fixture::RecordWidget::new(scene);
        widget.clip = false;
        widget.opacity = 0.5;
        let mut bridge = crate::test_support::r01_fixture::widget_bridge(widget, Affine::IDENTITY);
        let mut build = reel_ui::frame::FrameBuild::new(
            bridge.record(reel_core::identity::FrameId(1)).unwrap(),
            crate::test_support::r01_fixture::empty_snapshot(&pool),
            reel_ui::frame::FrameInfo {
                logical_size: [160., 120.],
                physical_size: [160, 120],
            },
            crate::test_support::r01_fixture::limits(),
            &pool,
        )
        .unwrap();
        while !build
            .step(crate::test_support::r01_fixture::step_budget())
            .unwrap()
            .complete
        {}
        let frame = build.finish().unwrap();
        let commands = frame.commands();
        let first = commands
            .iter()
            .position(|c| matches!(c.op, FrameOp::PushGroup(_)))
            .unwrap();
        let body = &commands[first + 1..];
        let call = |group: &FrameGroup, boundary| {
            invisible_group(
                group,
                body,
                Affine::IDENTITY,
                [160, 120],
                Scissor::full([160, 120]),
                boundary,
            )
        };
        let outside = group(InkBounds::Rect(Rect::new(2000., 0., 2400., 120.)));
        let consumed = call(&outside, false).unwrap();
        assert!(matches!(body[consumed - 1].op, FrameOp::PopGroup));
        assert!(
            body[..consumed - 1]
                .iter()
                .any(|c| matches!(c.op, FrameOp::PushGroup(_)))
        );
        assert_eq!(call(&outside, true), None);
        assert_eq!(call(&group(InkBounds::Unbounded), false), None);
        // A partly visible output still needs all source samples, including
        // thousands of pixels beyond the parent's right edge.
        assert_eq!(
            call(
                &group(InkBounds::Rect(Rect::new(150., 0., 2400., 120.))),
                false
            ),
            None
        );
        assert_eq!(
            call(
                &group(InkBounds::Rect(Rect::new(160.5, 0., 2400., 120.))),
                false
            ),
            None
        );
        assert_eq!(
            invisible_group(
                &outside,
                &body[..consumed - 1],
                Affine::IDENTITY,
                [160, 120],
                Scissor::full([160, 120]),
                false
            ),
            None
        );
    }
}
