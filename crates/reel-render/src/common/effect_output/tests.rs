//! Real effect scopes exercise frozen output views and their scratch liveness.
use super::*;
use crate::{
    common::{offscreen, target_plan},
    test_support::r01_fixture as fixture,
};
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
};
use reel_ui::{
    bridge::PresentationBridge,
    core::{PaintEffect, Widget},
    frame::{FrameBuild, FrameInfo, RenderFrame},
    imaging::{
        Composite,
        record::{Draw, Geometry, Scene},
    },
    kurbo::{Affine, Point, Rect, Size},
    peniko::{Brush, Color, ImageAlphaType, ImageBrush, ImageFormat},
    resources::{ColorEncoding, EffectShaderDescriptor, EffectShaderFormat, ResourceRegistry},
    stage::effect::*,
};

fn frame(source: [u32; 2]) -> (RenderFrame, PresentationBridge) {
    frame_with_image(source, false)
}

fn frame_with_image(source: [u32; 2], single_image: bool) -> (RenderFrame, PresentationBridge) {
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
    let shader =
        crate::effect::compile_fragment(&program, crate::effect::Language::Wgsl, 4096).unwrap();
    let descriptor = EffectShaderDescriptor::with_textures(
        [71; 32],
        EffectShaderFormat::Wgsl,
        vec![],
        0,
        true,
        EffectType::Vec4,
        shader.source.as_bytes(),
    )
    .unwrap()
    .with_source_footprint(
        crate::effect::source_footprint(&program),
        shader.source.as_bytes(),
    )
    .unwrap();
    let mut registry = ResourceRegistry::new(2, 1 << 20).unwrap();
    let bytes = shader.source.into_bytes();
    let ticket = pool.reserve(bytes.capacity() as u64).unwrap();
    let handle = registry
        .register_effect_shader(
            ResourceVersion {
                key: AssetKey("output-window".into()),
                version: ContentVersion(descriptor.shader_key),
            },
            descriptor,
            OwnedBytes::new(bytes, ticket).unwrap(),
        )
        .unwrap();
    let bounds = Rect::new(0., 0., f64::from(source[0]), f64::from(source[1]));
    let effect = PaintEffect {
        instance: EffectInstance::new([71; 32], program, vec![]).unwrap(),
        shader: registry.acquire(handle).unwrap(),
        images: vec![],
        bounds,
        coverage_invert: false,
        resolution_scale: 1.,
    };
    let mut scene = Scene::new();
    let mut handles = vec![handle];
    let brushes = if single_image {
        let image = registry
            .register_image(
                ResourceVersion {
                    key: AssetKey("deferred-output-source".into()),
                    version: ContentVersion([72; 32]),
                },
                OwnedBytes::new(vec![255; 4], pool.reserve(4).unwrap()).unwrap(),
                [1, 1],
                ImageFormat::Rgba8,
                ImageAlphaType::Alpha,
                ColorEncoding::Linear,
            )
            .unwrap();
        handles.push(image);
        vec![Brush::Image(ImageBrush::new(
            crate::test_support::image_data(&registry.acquire(image).unwrap()),
        ))]
    } else {
        vec![Brush::Solid(Color::WHITE), Brush::Solid(Color::TRANSPARENT)]
    };
    for brush in brushes {
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush,
            brush_transform: None,
            shape: Geometry::Rect(bounds),
            composite: Composite::default(),
        });
    }
    let mut widget = fixture::RecordWidget::new(scene);
    widget.clip = false;
    widget.effect = Some(effect.clone());
    for _ in 0..2 {
        let mut parent = fixture::RecordWidget::new(Scene::new());
        parent.clip = false;
        parent.effect = Some(effect.clone());
        parent.inner.add(
            widget.prepare(),
            Point::ZERO,
            Size::new(bounds.width(), bounds.height()),
        );
        widget = parent;
    }
    let mut stage = fixture::RecordWidget::new(Scene::new());
    stage.clip = true;
    stage.inner.add(
        widget.prepare(),
        Point::new(0., 96.),
        Size::new(bounds.width(), bounds.height()),
    );
    let mut bridge = fixture::widget_bridge(stage, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&handles, &pool).unwrap(),
        FrameInfo {
            logical_size: [640., 256.],
            physical_size: [640, 256],
        },
        fixture::limits(),
        &pool,
    )
    .unwrap();
    while !build.step(fixture::step_budget()).unwrap().complete {}
    (build.finish().unwrap(), bridge)
}

fn choose(frame: &RenderFrame) -> (offscreen::Plan, u64) {
    let (plan, bytes, _) = offscreen::Plan::choose(
        frame,
        None,
        [640, 256],
        false,
        1,
        4096,
        &Default::default(),
        64 << 20,
        &|size| Ok(u64::from(size[0]) * u64::from(size[1]) * 4),
        target_plan::MaskExecution::Inline,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    (plan, bytes)
}

#[test]
fn frozen_output_views_reduce_physical_lifetimes_without_changing_the_logical_grid() {
    let (frame, _owner) = frame([2048, 512]);
    let (full, full_bytes) = testing::without(|| choose(&frame));
    let (plan, bytes) = choose(&frame);
    assert!(bytes < full_bytes);
    let crops: Vec<_> = (0..frame.commands().len())
        .filter_map(|push| plan.output_crop(push))
        .collect();
    assert!(!crops.is_empty());
    for crop in crops {
        assert!(crop.original_coordinates());
        assert_eq!(crop.logical, [2048, 512]);
        assert_eq!(crop.canonical_storage(), full.storage_size(crop.logical));
        assert!(plan.requirements().exact().contains(&crop.size));
        assert!(plan.requirements().count(crop.size) > 0);
    }
    let input = target_plan::Input {
        frame: &frame,
        second: None,
        size: [640, 256],
        capture: false,
        samples: 1,
        max_dimension: 4096,
        glyphs: &Default::default(),
        schedule: &plan,
        masks: target_plan::MaskExecution::Inline,
        selection: &Default::default(),
        cost: &|size| Ok(u64::from(size[0]) * u64::from(size[1]) * 4),
        coverage_cost: None,
    };
    assert_eq!(input.physical_measure(|_| Ok(())).unwrap().bytes, bytes);
    let (again, repeated_bytes) = choose(&frame);
    assert_eq!(repeated_bytes, bytes);
    assert!(
        (0..frame.commands().len()).all(|push| again.output_crop(push) == plan.output_crop(push))
    );
}

#[test]
fn macro_compatible_chains_keep_their_original_complete_cells() {
    let (frame, _owner) = frame([512, 512]);
    let (plan, _) = choose(&frame);
    assert!((0..frame.commands().len()).all(|push| plan.output_crop(push).is_none()));
}

#[test]
fn physical_output_windows_keep_filter_neighbors_and_virtual_texel_phase() {
    let logical = [2048, 512];
    let demand = super::super::scissor::Scissor {
        x: 0,
        y: 0,
        width: 650,
        height: 170,
    };
    let crop = Crop::physical_window(logical, logical, demand).unwrap();
    assert!(demand.x + demand.width <= crop.size[0] && demand.y + demand.height <= crop.size[1]);
    let mut original = [[0.; 4]; crate::effect::CONTEXT_SLOTS];
    original[2] = [1., 0., 0., 0.];
    original[3] = [0., 1., 0., 0.];
    original[5] = [2048., 512., 2048., 512.];
    original[6] = [0., 0., 1., 1.];
    let mut mapped = original;
    crop.map(&mut mapped);
    for axis in 0..2 {
        for texel in [0.5, 1.5, f64::from(demand.width.min(demand.height)) - 0.5] {
            let uv = (texel / f64::from(logical[axis])) as f32;
            let a = uv * original[2 + axis][axis];
            let b = uv * mapped[2 + axis][axis];
            assert_eq!(
                f64::from(a) * f64::from(logical[axis]),
                f64::from(b) * f64::from(crop.size[axis])
            );
        }
    }
}

#[test]
fn cached_prefix_admits_the_frozen_map_before_cold_scratch_fallback() {
    use crate::common::{effect_inputs, effect_result::Token, glyph_plan};
    // Keep the whole producer visible. A larger offscreen image can instead
    // choose a translated logical crop, whose inputs intentionally stay outside
    // the original-grid cache proof exercised by this admission test.
    let (frame, _owner) = frame_with_image([64, 48], true);
    let cost = |size: [u32; 2]| Ok(u64::from(size[0]) * u64::from(size[1]) * 4);
    let glyphs = glyph_plan::Plan::default();
    let selection = Default::default();
    let ordinary = offscreen::Plan::default();
    fn input<'a>(
        frame: &'a RenderFrame,
        glyphs: &'a glyph_plan::Plan,
        schedule: &'a offscreen::Plan,
        selection: &'a crate::common::retained::Plan,
        cost: &'a dyn Fn([u32; 2]) -> Result<u64, crate::common::GpuError>,
    ) -> target_plan::Input<'a> {
        target_plan::Input {
            frame,
            second: None,
            size: [640, 256],
            capture: false,
            samples: 1,
            max_dimension: 4096,
            glyphs,
            schedule,
            masks: target_plan::MaskExecution::Inline,
            selection,
            cost,
            coverage_cost: None,
        }
    }
    let cold = input(&frame, &glyphs, &ordinary, &selection, &cost)
        .measure(|_| Ok(()))
        .unwrap()
        .bytes;
    let limit = cold - 1;
    let pool = fixture::pool();
    let mut collects = 0;
    let (mut schedule, bytes, _, selected) = offscreen::Plan::choose_with_groups(
        &frame,
        None,
        [640, 256],
        false,
        1,
        4096,
        &glyphs,
        limit,
        &cost,
        None,
        target_plan::MaskExecution::Inline,
        crate::common::blend::DestinationAccess::Sampled,
        &selection,
        None,
        None,
        |schedule, cold_bytes| {
            collects += 1;
            assert!(cold_bytes > limit);
            assert!((0..frame.commands().len()).all(|push| schedule.output_crop(push).is_none()));
            let mut owner = 0;
            let selected = input(&frame, &glyphs, schedule, &selection, &cost)
                .collect_effect_inputs(
                    [0, 1],
                    limit,
                    &pool,
                    &mut |proof| {
                        let bytes = cost(proof.grid().output_storage).unwrap();
                        let hit = effect_inputs::Hit {
                            token: Token(owner + 1, owner + 1),
                            owner,
                            bytes,
                        };
                        owner += 1;
                        Some(hit)
                    },
                    |_| Ok(()),
                )?;
            assert!(!selected.is_empty());
            Ok((selected.bytes(), selected))
        },
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(collects, 1, "winning inputs freeze the first admitted map");
    assert!(bytes <= limit);
    for entry in selected.entries() {
        assert_eq!(
            entry.grid.source_storage,
            schedule.storage_size(entry.grid.source_size)
        );
    }
    let requirements: Vec<_> = selected.requirements().iter().collect();
    schedule.set_effect_inputs(selected);
    assert_eq!(
        schedule.requirements().iter().collect::<Vec<_>>(),
        requirements
    );
    assert_eq!(
        input(&frame, &glyphs, &schedule, &selection, &cost)
            .physical_measure(|_| Ok(()))
            .unwrap()
            .bytes,
        bytes
    );
}

#[test]
fn physical_deferred_raw_sources_select_existing_inputs_on_the_original_grid() {
    use crate::common::{effect_inputs, effect_result::Token};
    let (frame, _owner) = frame_with_image([2048, 512], true);
    let (mut schedule, ordinary_bytes) = choose(&frame);
    let logical = [2048, 512];
    let virtual_storage = schedule.storage_size(logical);
    let source_size = (0..frame.commands().len())
        .filter_map(|push| schedule.output_crop(push))
        .next()
        .unwrap()
        .size;
    assert_ne!(source_size, virtual_storage);
    assert_eq!(
        schedule.requirements().count(virtual_storage),
        0,
        "the physical raw and intermediate writers already use cropped backing"
    );
    let bytes = u64::from(source_size[0]) * u64::from(source_size[1]) * 4;
    let mut lookups = 0;
    let selected = target_plan::Input {
        frame: &frame,
        second: None,
        size: [640, 256],
        capture: false,
        samples: 1,
        max_dimension: 4096,
        glyphs: &Default::default(),
        schedule: &schedule,
        masks: target_plan::MaskExecution::Inline,
        selection: &Default::default(),
        cost: &|size| Ok(u64::from(size[0]) * u64::from(size[1]) * 4),
        coverage_cost: None,
    }
    .collect_effect_inputs(
        [0, 1],
        ordinary_bytes,
        &fixture::pool(),
        &mut |proof| {
            let grid = proof.grid();
            assert_eq!(grid.output_size, logical);
            assert_eq!(grid.output_storage, virtual_storage);
            let owner = lookups;
            lookups += 1;
            Some(effect_inputs::Hit {
                token: Token(owner + 1, owner + 1),
                owner,
                bytes,
            })
        },
        |_| Ok(()),
    )
    .unwrap();
    assert!(lookups > 0 && !selected.is_empty());
    assert!(selected.bytes() <= ordinary_bytes);
    assert_eq!(selected.pinned_bytes(), bytes);
    let selected_bytes = selected.bytes();
    schedule.set_effect_inputs(selected);
    let input = target_plan::Input {
        frame: &frame,
        second: None,
        size: [640, 256],
        capture: false,
        samples: 1,
        max_dimension: 4096,
        glyphs: &Default::default(),
        schedule: &schedule,
        masks: target_plan::MaskExecution::Inline,
        selection: &Default::default(),
        cost: &|size| Ok(u64::from(size[0]) * u64::from(size[1]) * 4),
        coverage_cost: None,
    };
    assert_eq!(
        input.physical_measure(|_| Ok(())).unwrap().bytes,
        selected_bytes,
        "selected input pixels and remaining physical lifetimes share one budget"
    );
}
