use super::*;
use crate::test_support::r01_fixture as fixture;
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
};
use reel_ui::{
    core::{PaintEffect, Widget},
    frame::{FrameBuild, FrameInfo},
    imaging::record::{Draw, Scene},
    kurbo::{Point, Size},
    peniko::{Color, ImageAlphaType, ImageBrush, ImageFormat},
    resources::{ColorEncoding, EffectShaderDescriptor, EffectShaderFormat, ResourceRegistry},
    stage::effect::*,
};

pub(crate) fn repeated_sources(transform: Affine, different: bool) -> RenderFrame {
    let pool = fixture::pool();
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
        crate::effect::compile_fragment(&program, crate::effect::Language::Wgsl, 16384).unwrap();
    let descriptor = EffectShaderDescriptor::with_textures(
        [67; 32],
        EffectShaderFormat::Wgsl,
        vec![],
        0,
        true,
        EffectType::Vec4,
        fragment.source.as_bytes(),
    )
    .unwrap();
    let mut registry = ResourceRegistry::new(2, 1 << 20).unwrap();
    let source = fragment.source.into_bytes();
    let ticket = pool.reserve(source.capacity() as u64).unwrap();
    let shader = registry
        .register_effect_shader(
            ResourceVersion {
                key: AssetKey("repeated-effect".into()),
                version: ContentVersion(descriptor.shader_key),
            },
            descriptor,
            OwnedBytes::new(source, ticket).unwrap(),
        )
        .unwrap();
    let pixels: Vec<_> = (0u8..64)
        .flat_map(|i| {
            [
                i.wrapping_mul(29),
                i.wrapping_mul(43),
                i.wrapping_mul(71),
                if i % 3 == 0 { 0 } else { 211 },
            ]
        })
        .collect();
    let image = registry
        .register_image(
            ResourceVersion {
                key: AssetKey("repeated-image".into()),
                version: ContentVersion([68; 32]),
            },
            OwnedBytes::new(pixels, pool.reserve(256).unwrap()).unwrap(),
            [8, 8],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    let image_data = crate::test_support::image_data(&registry.acquire(image).unwrap());
    let bounds = Rect::new(0., 0., 96., 64.);
    let effect = PaintEffect {
        instance: EffectInstance::new([67; 32], program, vec![]).unwrap(),
        shader: registry.acquire(shader).unwrap(),
        images: vec![],
        bounds,
        coverage_invert: false,
        resolution_scale: 1.,
    };
    let fill = |scene: &mut Scene, rect: Rect, color| {
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Solid(color),
            brush_transform: None,
            shape: Geometry::Rect(rect),
            composite: Default::default(),
        })
    };
    let mut root = fixture::RecordWidget::new(Scene::new());
    root.clip = false;
    fill(
        &mut root.own,
        Rect::new(0., 0., 256., 192.),
        Color::from_rgba8(30, 50, 80, 255),
    );
    for index in 0..2 {
        let mut leaf = fixture::RecordWidget::new(Scene::new());
        leaf.clip = false;
        leaf.own.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Image(ImageBrush::new(image_data.clone())),
            brush_transform: Some(Affine::scale_non_uniform(96. / 8., 64. / 8.)),
            shape: Geometry::Rect(bounds),
            composite: Default::default(),
        });
        fill(
            &mut leaf.own,
            bounds,
            Color::from_rgba8(
                if different && index == 1 { 200 } else { 80 },
                170,
                220,
                180,
            ),
        );
        let mut source = leaf;
        for _ in 0..4 {
            let mut parent = fixture::RecordWidget::new(Scene::new());
            parent.clip = false;
            parent.effect = Some(effect.clone());
            parent
                .inner
                .add(source.prepare(), Point::ZERO, Size::new(96., 64.));
            source = parent;
        }
        let mut outer = fixture::RecordWidget::new(Scene::new());
        outer.clip = false;
        outer.opacity = 0.6;
        outer
            .inner
            .add(source.prepare(), Point::ZERO, Size::new(96., 64.));
        root.inner
            .add(outer.prepare(), Point::new(24.3, 22.5), Size::new(96., 64.));
        if index == 0 {
            let mut middle = fixture::RecordWidget::new(Scene::new());
            middle.clip = false;
            fill(
                &mut middle.own,
                Rect::new(0., 0., 32., 32.),
                Color::from_rgba8(230, 30, 50, 140),
            );
            root.inner
                .add(middle.prepare(), Point::new(50., 35.), Size::new(32., 32.));
        }
    }
    let mut bridge = fixture::widget_bridge(root, transform);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[shader, image], &pool).unwrap(),
        FrameInfo {
            logical_size: [256., 192.],
            physical_size: [256, 192],
        },
        fixture::limits(),
        &pool,
    )
    .unwrap();
    while !build.step(fixture::step_budget()).unwrap().complete {}
    build.finish().unwrap()
}

fn plan(frame: &RenderFrame) -> (Plan, u64) {
    let (plan, bytes, _) = super::super::test_support::without_atlas(|| {
        Plan::choose(
            frame,
            None,
            [256, 192],
            false,
            1,
            4096,
            &Default::default(),
            u64::MAX,
            &|s| Ok(u64::from(s[0]) * u64::from(s[1]) * 4),
            crate::common::target_plan::MaskExecution::Inline,
            &Default::default(),
            |_| Ok(()),
        )
        .unwrap()
    });
    (plan, bytes)
}

#[test]
fn duplicate_last_source_keeps_both_parent_reads_and_one_physical_lifetime() {
    let frame = repeated_sources(Affine::IDENTITY, false);
    let (before, old_bytes) = testing::without(|| plan(&frame));
    let (after, bytes) = plan(&frame);
    assert_eq!(after.entries.len(), 2);
    let first = after.entries[0];
    let last = after.entries[1];
    assert!(last.last && !first.last);
    assert_eq!(last.canonical_push, Some(first.push));
    assert!(first.canonical_push.is_none());
    assert!(
        bytes < old_bytes,
        "duplicate production must lower peak target bytes"
    );
    assert_eq!(
        after.storage_size(first.source_size),
        before.storage_size(first.source_size)
    );
    let mut cursor = after.parent_cursor(first.push);
    let mut sources = Vec::new();
    let mut draws = Vec::new();
    while let Some(step) = after.next_parent(frame.commands(), &mut cursor) {
        match step {
            ParentStep::Sources(range) => sources.extend(range),
            ParentStep::Draws(range) => draws.extend(range),
        }
    }
    assert_eq!(sources, [0, 1]);
    assert!(
        !draws.is_empty(),
        "interleaved parent paint stays between reads"
    );
}

#[test]
fn changed_source_pixels_and_physical_windows_do_not_alias() {
    let changed = repeated_sources(Affine::IDENTITY, true);
    assert!(
        plan(&changed)
            .0
            .entries
            .iter()
            .all(|e| e.canonical_push.is_none())
    );
    let frame = repeated_sources(Affine::IDENTITY, false);
    let (mut candidate, _) = testing::without(|| plan(&frame));
    let a = candidate.entries[0];
    let b = candidate.entries[1];
    candidate
        .output_crops
        .entries
        .push(crate::common::effect_output::Entry {
            push: b.push + 1,
            crop: crate::common::source_crop::Crop {
                logical: [96, 64],
                origin: [0, 0],
                size: [64, 64],
                physical: Some([128, 64]),
            },
        });
    candidate
        .output_crops
        .entries
        .sort_unstable_by_key(|e| e.push);
    assert!(!candidate.same_source(frame.drawing(), &Default::default(), a, b));
}

#[test]
fn equal_full_size_views_keep_their_distinct_parent_scissors() {
    let frame = repeated_sources(Affine::IDENTITY, false);
    let (mut candidate, _) = testing::without(|| plan(&frame));
    // Near-full rectangles can round to the complete target capacity, leaving
    // both views absent even though one parent excludes the first pixel column.
    for entry in &mut candidate.entries {
        entry.view = None;
    }
    candidate.entries[1].parent_scissor.x += 1;
    candidate.entries[1].parent_scissor.width -= 1;
    assert!(!candidate.reuse_sources(frame.drawing(), &Default::default()));
    assert!(
        candidate
            .entries
            .iter()
            .all(|entry| entry.canonical_push.is_none())
    );
}
