use super::*;
use reel_ui::{
    core::Widget,
    frame::{CommandOrigin, FrameEffect, GroupComposite, InkBounds, LocalId, RecordLocation},
    imaging::{Composite, record::Geometry},
    kurbo::{Affine, Rect},
    peniko::{Brush, Color},
    resources::ResourceHandle,
};

fn group() -> FrameGroup {
    FrameGroup {
        owner: reel_ui::widgets::Label::new("").prepare().id(),
        effect: Some(FrameEffect {
            definition: [0; 32],
            shader: ResourceHandle {
                registry: 1,
                slot: 0,
                generation: 1,
            },
            values: vec![],
            images: vec![],
            bounds: Rect::new(0., 0., 137., 93.),
            coverage_invert: false,
            resolution_scale: 1.,
        }),
        effect_to_target: Affine::IDENTITY,
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
        ink_bounds: InkBounds::Unbounded,
    }
}
fn commands(count: usize) -> Vec<FrameCommand> {
    let prototype = group();
    let origin = CommandOrigin {
        source: 0,
        local: LocalId::None,
        at: RecordLocation {
            fragment: 0,
            command: 0,
            mask_depth: 0,
            owner: prototype.owner,
        },
    };
    let command = |op| FrameCommand { origin, op };
    let mut commands: Vec<_> = (0..count)
        .map(|_| command(FrameOp::PushGroup(prototype.clone())))
        .collect();
    commands.push(command(FrameOp::Draw(FrameDraw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::WHITE),
        brush_transform: None,
        shape: Geometry::Rect(Rect::new(0., 0., 137., 93.)),
        composite: Composite::default(),
    })));
    commands.extend((0..count).map(|_| command(FrameOp::PopGroup)));
    commands
}
fn planned(commands: &[FrameCommand], root: usize) -> Option<Chain> {
    let groups = GroupPlan::new(
        commands,
        Some(0),
        std::iter::empty(),
        std::iter::empty(),
        |_| Ok(()),
    )
    .unwrap();
    plan(commands, root, &groups)
}

#[test]
fn bounded_chain_keeps_the_outer_effect_and_orders_inner_unorm_stages() {
    assert_eq!(
        planned(&commands(2), 0),
        Some(Chain {
            effects: [1, 0, 0],
            count: 1
        })
    );
    assert_eq!(
        planned(&commands(4), 0),
        Some(Chain {
            effects: [3, 2, 1],
            count: 3
        })
    );
    assert_eq!(planned(&commands(1), 0), None);
    let long = commands(5);
    assert_eq!(planned(&long, 0), None);
    assert_eq!(
        planned(&long, 1),
        Some(Chain {
            effects: [4, 3, 2],
            count: 3
        })
    );
}

#[test]
fn the_original_inline_source_group_is_kept_inside_cell_zero() {
    let mut commands = commands(4);
    let mut source = group();
    source.effect = None;
    let mut begin = commands[0].clone();
    begin.op = FrameOp::PushGroup(source);
    let mut end = commands[0].clone();
    end.op = FrameOp::PopGroup;
    commands.insert(4, begin);
    commands.insert(6, end);
    assert_eq!(
        planned(&commands, 0),
        Some(Chain {
            effects: [3, 2, 1],
            count: 3
        })
    );
    let groups = GroupPlan::new(
        &commands,
        Some(0),
        std::iter::once(2..4),
        std::iter::empty(),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(plan(&commands, 0, &groups), None);
    let FrameOp::PushGroup(source) = &mut commands[4].op else {
        unreachable!()
    };
    source.composite.alpha = 0.5;
    assert_eq!(
        planned(&commands, 0),
        None,
        "source isolation remains a real pass"
    );
}

#[test]
fn chained_effects_require_identical_domains_and_original_intermediate_composites() {
    for change in 0..6 {
        let mut commands = commands(4);
        let FrameOp::PushGroup(child) = &mut commands[2].op else {
            unreachable!()
        };
        match change {
            0 => child.composite.alpha = 0.5,
            1 => child.invert = 0.5,
            2 => child.effect_to_target = Affine::translate((0.25, 0.)),
            3 => child.effect.as_mut().unwrap().bounds.x1 += 1.,
            4 => child.mask_effect = child.effect.clone(),
            5 => child.composite.blend = reel_ui::peniko::Mix::Screen.into(),
            _ => unreachable!(),
        }
        assert_eq!(planned(&commands, 0), None, "change {change}");
    }
    let mut commands = commands(4);
    let FrameOp::PushGroup(root) = &mut commands[0].op else {
        unreachable!()
    };
    root.composite.alpha = 0.25;
    assert!(
        planned(&commands, 0).is_some(),
        "outer opacity stays in the original final pass"
    );
}

#[test]
fn a_sibling_or_nonordinary_leaf_is_not_a_single_consumer_chain() {
    let mut sibling = commands(4);
    sibling.insert(8, sibling[4].clone());
    assert_eq!(planned(&sibling, 0), None);
    let mut backdrop = commands(4);
    let FrameOp::Draw(FrameDraw::Fill { composite, .. }) = &mut backdrop[4].op else {
        unreachable!()
    };
    composite.blend = reel_ui::peniko::Mix::Multiply.into();
    assert_eq!(planned(&backdrop, 0), None);
    let mut clip = commands(4);
    let FrameOp::PushGroup(group) = &mut clip[3].op else {
        unreachable!()
    };
    group.mask = Some(reel_ui::frame::MaskUse {
        mask: 0,
        transform: Affine::IDENTITY,
    });
    assert_eq!(planned(&clip, 0), None);
}

#[test]
fn the_cell_grid_preserves_each_source_extent_and_rejects_oversized_sources() {
    assert_eq!(layout([27, 84], 4), Some(([128, 128], [2, 2])));
    assert_eq!(layout([832, 288], 3), Some(([896, 384], [2, 2])));
    assert_eq!(layout([960, 544], 2), Some(([1024, 640], [1, 2])));
    for (size, cells) in [
        ([1920, 400], 4),
        ([1025, 1], 2),
        ([1, 0], 2),
        ([u32::MAX, 1], 3),
        ([1, 1], 1),
        ([1, 1], 5),
    ] {
        assert_eq!(layout(size, cells), None);
    }
}
