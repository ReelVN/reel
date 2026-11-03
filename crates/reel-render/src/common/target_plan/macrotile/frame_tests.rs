//! Real recording/layout fixtures exercise the liveness annotations themselves.
use super::*;
use crate::test_support::r01_fixture;
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
};
use reel_ui::{
    core::{PaintEffect, Widget},
    frame::{FrameBuild, FrameInfo},
    imaging::{
        Composite,
        record::{Draw, Geometry, Scene},
    },
    kurbo::{Point, Rect, Size},
    peniko::{Brush, Color, ImageAlphaType, ImageBrush, ImageFormat},
    resources::{ColorEncoding, EffectShaderDescriptor, EffectShaderFormat, ResourceRegistry},
    stage::effect::*,
};

#[derive(Clone, Copy)]
enum Boundary {
    None,
    Fork,
    Mask,
    Capture,
    Alpha,
}

fn fill(size: [u32; 2]) -> Scene {
    let mut scene = Scene::new();
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::WHITE),
        brush_transform: None,
        shape: Geometry::Rect(Rect::new(0., 0., size[0] as f64, size[1] as f64)),
        composite: Composite::default(),
    });
    scene
}

fn make_frame(chains: &[(usize, [u32; 2], Boundary)]) -> RenderFrame {
    make_frame_with_pattern(chains, false).0
}

pub(crate) fn warm_terminal_sources() -> (RenderFrame, reel_ui::bridge::PresentationBridge) {
    make_frame_with_pattern(&[(4, [137, 93], Boundary::None); 3], true)
}

fn make_frame_with_pattern(
    chains: &[(usize, [u32; 2], Boundary)],
    patterned: bool,
) -> (RenderFrame, reel_ui::bridge::PresentationBridge) {
    let pool = r01_fixture::pool();
    let mut registry = ResourceRegistry::new(if patterned { 3 } else { 2 }, 16384).unwrap();
    let mut handles = Vec::new();
    let mut paints = Vec::new();
    for scalar in [false, true] {
        let nodes = if scalar {
            vec![EffectNode {
                ty: EffectType::Float,
                op: EffectOp::Constant(EffectValue::Float(0.5f32.to_bits())),
            }]
        } else {
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
            ]
        };
        let result = EffectNodeId((nodes.len() - 1) as u32);
        let program = EffectProgram::new(vec![], nodes, result).unwrap();
        let fragment =
            crate::effect::compile_fragment(&program, crate::effect::Language::Wgsl, 16384)
                .unwrap();
        let definition = [if scalar { 5 } else { 4 }; 32];
        let descriptor = EffectShaderDescriptor::with_textures(
            definition,
            EffectShaderFormat::Wgsl,
            vec![],
            0,
            !scalar,
            if scalar {
                EffectType::Float
            } else {
                EffectType::Vec4
            },
            fragment.source.as_bytes(),
        )
        .unwrap();
        let handle = registry
            .register_effect_shader(
                ResourceVersion {
                    key: AssetKey(format!("selected-macro-{scalar}")),
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
        handles.push(handle);
        paints.push(PaintEffect {
            instance: EffectInstance::new(definition, program, vec![]).unwrap(),
            shader: registry.acquire(handle).unwrap(),
            images: vec![],
            bounds: Rect::new(0., 0., 137., 93.),
            coverage_invert: false,
            resolution_scale: 1.,
        });
    }
    let image = patterned.then(|| {
        let bytes = (0u8..64)
            .flat_map(|index| {
                [
                    index.wrapping_mul(29),
                    index.wrapping_mul(43),
                    index.wrapping_mul(71),
                    if index % 3 == 0 { 0 } else { 211 },
                ]
            })
            .collect::<Vec<_>>();
        let ticket = pool.reserve(bytes.capacity() as u64).unwrap();
        let handle = registry
            .register_image(
                ResourceVersion {
                    key: AssetKey("warm-terminal-source".into()),
                    version: ContentVersion([49; 32]),
                },
                OwnedBytes::new(bytes, ticket).unwrap(),
                [8, 8],
                ImageFormat::Rgba8,
                ImageAlphaType::Alpha,
                ColorEncoding::Linear,
            )
            .unwrap();
        handles.push(handle);
        registry.acquire(handle).unwrap()
    });
    let mut root = r01_fixture::RecordWidget::new(Scene::new());
    root.clip = false;
    for (position, &(depth, size, boundary)) in chains.iter().enumerate() {
        let mut color = paints[0].clone();
        color.bounds = Rect::new(0., 0., size[0] as f64, size[1] as f64);
        let scene = if let Some(image) = &image {
            let mut scene = Scene::new();
            scene.draw(Draw::Fill {
                transform: Affine::IDENTITY,
                fill_rule: Default::default(),
                brush: Brush::Image(ImageBrush::new(crate::test_support::image_data(image))),
                brush_transform: None,
                shape: Geometry::Rect(color.bounds),
                composite: Composite::default(),
            });
            scene
        } else {
            fill(size)
        };
        let mut widget = r01_fixture::RecordWidget::new(scene);
        widget.clip = false;
        widget.effect = Some(color.clone());
        if matches!(boundary, Boundary::Alpha) {
            widget.opacity = 0.5;
        }
        for _ in 1..depth {
            let mut parent = r01_fixture::RecordWidget::new(Scene::new());
            parent.clip = false;
            parent.effect = Some(color.clone());
            parent.inner.add(
                widget.prepare(),
                Point::ZERO,
                Size::new(size[0] as f64, size[1] as f64),
            );
            widget = parent;
        }
        match boundary {
            Boundary::Fork => widget.post = fill(size),
            Boundary::Mask => {
                let mut mask = paints[1].clone();
                mask.bounds = color.bounds;
                widget.mask_effect = Some(mask);
            }
            Boundary::Capture => widget.capture_included = false,
            _ => {}
        }
        root.inner.add(
            widget.prepare(),
            Point::new(position as f64 * 180., 0.),
            Size::new(size[0] as f64, size[1] as f64),
        );
    }
    let mut bridge = r01_fixture::widget_bridge(root, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&handles, &pool).unwrap(),
        FrameInfo {
            logical_size: [640., 480.],
            physical_size: [640, 480],
        },
        r01_fixture::limits(),
        &pool,
    )
    .unwrap();
    while !build.step(r01_fixture::step_budget()).unwrap().complete {}
    (build.finish().unwrap(), bridge)
}

fn choose(frame: &RenderFrame) -> (u64, Option<Selection>) {
    let glyphs = crate::common::glyph_plan::Plan::default();
    let schedule = offscreen::Plan::default();
    let selection = Default::default();
    let input = Input {
        frame,
        second: None,
        size: [640, 480],
        capture: false,
        samples: 1,
        max_dimension: 4096,
        glyphs: &glyphs,
        schedule: &schedule,
        masks: MaskExecution::Sampled,
        selection: &selection,
        cost: &|size| Ok(size[0] as u64 * size[1] as u64 * 4),
        coverage_cost: None,
    };
    (
        input.measure(|_| Ok(())).unwrap().bytes,
        input.macrotile_selection(u64::MAX, |_| Ok(())).unwrap(),
    )
}

#[test]
fn recorded_terminal_window_keeps_sources_until_consumption_and_shares_continuous_cells() {
    let frame = make_frame(&[(4, [137, 93], Boundary::None); 3]);
    let cost = |size: [u32; 2]| Ok(u64::from(size[0]) * u64::from(size[1]) * 4);
    let glyphs = crate::common::glyph_plan::Plan::default();
    let selection = Default::default();
    let (schedule, _, _) = offscreen::Plan::choose(
        &frame,
        None,
        [640, 480],
        false,
        1,
        4096,
        &glyphs,
        u64::MAX,
        &cost,
        MaskExecution::Sampled,
        &selection,
        |_| Ok(()),
    )
    .unwrap();
    let sources: Vec<_> = (0..frame.commands().len())
        .filter_map(|index| schedule.entry(index))
        .collect();
    assert_eq!(sources.len(), 3);
    assert!(
        sources
            .iter()
            .all(|entry| entry.view.is_none() && !entry.shared)
    );
    let input = Input {
        frame: &frame,
        second: None,
        size: [640, 480],
        capture: false,
        samples: 1,
        max_dimension: 4096,
        glyphs: &glyphs,
        schedule: &schedule,
        masks: MaskExecution::Sampled,
        selection: &selection,
        cost: &cost,
        coverage_cost: None,
    };
    let planned = input
        .macrotile_selection(u64::MAX, |_| Ok(()))
        .unwrap()
        .unwrap();
    assert_eq!(planned.entries.len(), 3);
    assert_eq!(
        planned
            .entries
            .iter()
            .map(|entry| entry.first_cell)
            .collect::<Vec<_>>(),
        [0, 4, 8]
    );
    assert!(
        planned
            .entries
            .iter()
            .all(|entry| entry.owner == 0 && entry.owner_cells == 12)
    );
    assert!(
        planned
            .entries
            .iter()
            .all(|entry| entry.release >= planned.entries[2].events.end)
    );
    assert_eq!(planned.requirements.count([137, 93]), 0);
    assert_eq!(planned.requirements.count([640, 480]), 1);
    assert_eq!(
        planned.bytes,
        cost([640, 480]).unwrap() + cost(planned.entries[0].extent()).unwrap()
    );
}

#[test]
fn recorded_two_three_four_effect_chains_keep_only_the_parent_ordinary_target() {
    for depth in 2..=4 {
        let frame = make_frame(&[(depth, [137, 93], Boundary::None)]);
        let (_, selected) = choose(&frame);
        let selected = selected.unwrap();
        assert_eq!(selected.entries.len(), 1);
        let entry = &selected.entries[0];
        assert_eq!(entry.count, depth - 1);
        assert_eq!(entry.logical, [137, 93]);
        assert_eq!(entry.storage, [137, 93]);
        assert_eq!(selected.requirements.count([640, 480]), 1);
        assert_eq!(selected.requirements.count([137, 93]), 0);
        assert_eq!(
            entry.key,
            Key::new(frame.drawing(), frame.commands(), entry.key.root)
        );
        assert!(matches!(
            frame.commands()[entry.key.root].op,
            FrameOp::PushGroup(_)
        ));
        let mut stack = Vec::new();
        let mut completed = Vec::new();
        for (index, command) in frame.commands().iter().enumerate().skip(entry.key.root) {
            match command.op {
                FrameOp::PushGroup(_) => stack.push(index),
                FrameOp::PopGroup => {
                    let push = stack.pop().unwrap();
                    if push == entry.key.root {
                        break;
                    }
                    if matches!(&frame.commands()[push].op, FrameOp::PushGroup(group) if group.effect.is_some())
                    {
                        completed.push(push);
                    }
                }
                _ => {}
            }
        }
        assert_eq!(
            &entry.effects[..entry.count],
            completed.as_slice(),
            "cell shaders follow the actual group completion order"
        );
    }
}

#[test]
fn recorded_fork_mask_alpha_and_capture_boundaries_keep_ordinary_storage() {
    for boundary in [
        Boundary::Fork,
        Boundary::Mask,
        Boundary::Alpha,
        Boundary::Capture,
    ] {
        let frame = make_frame(&[(2, [137, 93], boundary)]);
        let (ordinary, selected) = choose(&frame);
        assert!(ordinary > 640 * 480 * 4);
        assert!(selected.is_none());
    }
}

#[test]
fn an_adjacent_rejected_chain_keeps_the_same_storage_bucket_peak() {
    let frame = make_frame(&[
        (3, [137, 93], Boundary::None),
        (2, [137, 93], Boundary::Alpha),
    ]);
    let (ordinary, selected) = choose(&frame);
    let selected = selected.unwrap();
    assert_eq!(selected.entries.len(), 1);
    assert_eq!(selected.requirements.count([137, 93]), 2);
    let extent = selected.entries[0].extent();
    assert_eq!(
        selected.bytes,
        ordinary + extent[0] as u64 * extent[1] as u64 * 4
    );
}

#[test]
fn multiple_recorded_chains_remove_only_the_replaced_size_classes() {
    let frame = make_frame(&[
        (4, [137, 93], Boundary::None),
        (3, [65, 37], Boundary::None),
        (1, [95, 65], Boundary::None),
    ]);
    let (ordinary, selected) = choose(&frame);
    let selected = selected.unwrap();
    assert_eq!(selected.entries.len(), 2);
    assert_eq!(selected.requirements.count([137, 93]), 0);
    assert_eq!(selected.requirements.count([65, 37]), 0);
    assert_eq!(selected.requirements.count([95, 65]), 1);
    let atlas_bytes: u64 = selected
        .entries
        .iter()
        .map(|entry| entry.extent()[0] as u64 * entry.extent()[1] as u64 * 4)
        .sum();
    assert_eq!(
        ordinary + atlas_bytes - selected.bytes,
        (137 * 93 + 65 * 37) * 8
    );
    let other = make_frame(&[(4, [137, 93], Boundary::None)]);
    assert_ne!(
        selected.entries[0].key,
        choose(&other).1.unwrap().entries[0].key
    );
}

#[test]
fn sequential_chains_share_one_whole_atlas_and_only_the_last_can_be_cached() {
    let frame = make_frame(&[
        (4, [137, 93], Boundary::None),
        (2, [137, 93], Boundary::Alpha),
        (4, [137, 93], Boundary::None),
        (4, [137, 93], Boundary::None),
    ]);
    let (_, selected) = choose(&frame);
    let selected = selected.unwrap();
    assert_eq!(selected.entries.len(), 3);
    assert_eq!(
        selected.entries.iter().map(|e| e.owner).collect::<Vec<_>>(),
        [0, 0, 0]
    );
    assert_eq!(
        selected
            .entries
            .iter()
            .map(|e| e.last_use)
            .collect::<Vec<_>>(),
        [false, false, true]
    );
    assert!(
        selected
            .entries
            .windows(2)
            .all(|pair| pair[0].events.end <= pair[1].events.start)
    );
    // The rejected alpha chain retains its complete ordinary scratch demand.
    assert_eq!(selected.requirements.count([137, 93]), 2);
    let ordinary: u64 = selected
        .requirements
        .iter()
        .map(|(s, n)| u64::from(s[0]) * u64::from(s[1]) * 4 * n)
        .sum();
    let extent = selected.entries[0].extent();
    assert_eq!(
        selected.bytes,
        ordinary + u64::from(extent[0]) * u64::from(extent[1]) * 4
    );
}

#[test]
fn equal_atlas_extents_keep_distinct_cell_views_in_distinct_owners() {
    let frame = make_frame(&[
        (4, [137, 93], Boundary::None),
        (4, [150, 94], Boundary::None),
        (4, [137, 93], Boundary::None),
    ]);
    let (_, selected) = choose(&frame);
    let selected = selected.unwrap();
    assert_eq!(selected.entries[0].extent(), selected.entries[1].extent());
    assert_eq!(
        selected.entries.iter().map(|e| e.owner).collect::<Vec<_>>(),
        [0, 1, 0]
    );
    assert_eq!(
        selected
            .entries
            .iter()
            .map(|e| e.last_use)
            .collect::<Vec<_>>(),
        [false, true, true]
    );
}
