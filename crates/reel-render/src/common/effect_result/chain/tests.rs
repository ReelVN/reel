use super::*;
use crate::test_support::r01_fixture as fixture;
use reel_core::{budget::OwnedBytes, identity::*};
use reel_ui::{
    bridge::PresentationBridge,
    core::{PaintEffect, Widget},
    frame::{FrameBuild, FrameInfo, RenderFrame},
    imaging::record::{Draw, Geometry, Scene},
    kurbo::{Point, Size},
    peniko::{Color, ImageAlphaType, ImageFormat},
    resources::{ColorEncoding, EffectShaderDescriptor, EffectShaderFormat, ResourceRegistry},
    stage::effect::*,
};

fn frame(extra_draw: bool, different_domain: bool) -> (RenderFrame, PresentationBridge) {
    let pool = fixture::pool();
    let program = EffectProgram::new(
        vec![EffectParameter {
            name: "amount".into(),
            ty: EffectType::Float,
            default: None,
        }],
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
        [83; 32],
        EffectShaderFormat::Wgsl,
        vec![EffectType::Float],
        0,
        true,
        EffectType::Vec4,
        fragment.source.as_bytes(),
    )
    .unwrap();
    let mut registry = ResourceRegistry::new(4, 1 << 20).unwrap();
    let shader = registry
        .register_effect_shader(
            ResourceVersion {
                key: AssetKey("chain-shader".into()),
                version: ContentVersion(descriptor.shader_key),
            },
            descriptor,
            OwnedBytes::new(
                fragment.source.as_bytes().to_vec(),
                pool.reserve(fragment.source.len() as u64).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let image = registry
        .register_image(
            ResourceVersion {
                key: AssetKey("chain-image".into()),
                version: ContentVersion([84; 32]),
            },
            OwnedBytes::new(vec![255; 4], pool.reserve(4).unwrap()).unwrap(),
            [1, 1],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Linear,
        )
        .unwrap();
    let paint = PaintEffect {
        instance: EffectInstance::new([83; 32], program, vec![EffectValue::Float(0f32.to_bits())])
            .unwrap(),
        shader: registry.acquire(shader).unwrap(),
        images: vec![],
        bounds: Rect::new(0., 0., 32., 32.),
        coverage_invert: false,
        resolution_scale: 1.,
    };
    let mut scene = Scene::new();
    scene.push_group(Default::default());
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Image(ImageBrush::new(crate::test_support::image_data(
            &registry.acquire(image).unwrap(),
        ))),
        brush_transform: None,
        shape: Geometry::Rect(paint.bounds),
        composite: Default::default(),
    });
    scene.pop_group();
    let mut widget = fixture::RecordWidget::new(scene);
    widget.clip = false;
    widget.effect = Some(paint.clone());
    for level in 0..2 {
        let mut parent = fixture::RecordWidget::new(Scene::new());
        parent.clip = false;
        parent.effect = Some(paint.clone());
        if different_domain && level == 0 {
            parent.effect.as_mut().unwrap().bounds.x1 = 31.;
        }
        if extra_draw && level == 1 {
            parent.own.draw(Draw::Fill {
                transform: Affine::IDENTITY,
                fill_rule: Default::default(),
                brush: Brush::Solid(Color::WHITE),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(1., 1., 2., 2.)),
                composite: Default::default(),
            });
        }
        parent
            .inner
            .add(widget.prepare(), Point::ZERO, Size::new(32., 32.));
        widget = parent;
    }
    let mut bridge = fixture::widget_bridge(widget, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[shader, image], &pool).unwrap(),
        FrameInfo {
            logical_size: [32.; 2],
            physical_size: [32; 2],
        },
        fixture::limits(),
        &pool,
    )
    .unwrap();
    while !build.step(fixture::step_budget()).unwrap().complete {}
    // RecordingSource retains weak identities; the producer owns their scenes.
    (build.finish().unwrap(), bridge)
}

fn effects(drawing: &FrameDrawing) -> Vec<usize> {
    drawing
        .commands()
        .iter()
        .enumerate()
        .filter_map(|(index, command)| {
            matches!(&command.op, FrameOp::PushGroup(group) if group.effect.is_some())
                .then_some(index)
        })
        .collect()
}

fn full_grid(effect: &FrameEffect) -> Grid {
    super::super::tests::grid().color_context(effect)
}

fn effect(drawing: &FrameDrawing, at: usize) -> &FrameEffect {
    let FrameOp::PushGroup(group) = &drawing.commands()[at].op else {
        unreachable!()
    };
    group.effect.as_ref().unwrap()
}

fn groups(drawing: &FrameDrawing) -> GroupPlan {
    GroupPlan::new(
        drawing.commands(),
        Some(0),
        std::iter::empty(),
        std::iter::empty(),
        |_| Ok(()),
    )
    .unwrap()
}

fn seed(cache: &Cache<Arc<u32>>, drawing: &FrameDrawing, pool: &BudgetPool) -> (Token, Token) {
    let nodes = effects(drawing);
    assert_eq!(nodes.len(), 3);
    let raw_effect = effect(drawing, nodes[2]);
    let raw = Probe::new(
        drawing,
        nodes[2]..source_end(drawing.commands(), nodes[2]).unwrap(),
        raw_effect,
        full_grid(raw_effect),
        true,
    )
    .unwrap();
    let first = cache.prepare(raw, pool).unwrap();
    let first_token = first.token;
    assert!(cache.publish(first.observe(), pool));
    let middle = effect(drawing, nodes[1]);
    let second = cache
        .prepare(
            Probe::from_output(first_token, middle, full_grid(middle)),
            pool,
        )
        .unwrap();
    let second_token = second.token;
    assert!(cache.publish(second.observe(), pool));
    let root = effect(drawing, nodes[0]);
    let last = cache
        .prepare(
            Probe::from_output(second_token, root, full_grid(root)),
            pool,
        )
        .unwrap();
    assert!(cache.publish(last.finish(Arc::new(73)), pool));
    (first_token, second_token)
}

#[test]
fn metadata_only_prefix_resolves_the_retained_tail_without_materializing_inputs() {
    let (frame, _producer) = frame(false, false);
    let drawing = frame.drawing();
    let pool = fixture::pool();
    let cache = Cache::default();
    let (first, _) = seed(&cache, drawing, &pool);
    let nodes = effects(drawing);
    let root = effect(drawing, nodes[0]);
    let range = nodes[0]..source_end(drawing.commands(), nodes[0]).unwrap();
    let before = pool.usage().bytes;
    let resolved = probe(
        &cache,
        drawing,
        range,
        full_grid(root),
        &groups(drawing),
        &|size| size,
        &pool,
    )
    .unwrap();
    assert_eq!(*cache.lookup(&resolved).unwrap().1, 73);
    assert_eq!(
        pool.usage().bytes,
        before,
        "the temporary layer stack releases its reservation"
    );
    let bottom = effect(drawing, nodes[2]);
    let raw = Probe::new(
        drawing,
        nodes[2]..source_end(drawing.commands(), nodes[2]).unwrap(),
        bottom,
        full_grid(bottom),
        true,
    )
    .unwrap();
    assert_eq!(cache.lookup_content(&raw), Some(first));
    assert!(
        cache.lookup(&raw).is_none(),
        "the prefix kept identity without pixels"
    );
}

#[test]
fn changed_inner_parameters_or_raster_inputs_keep_ordinary_replay() {
    let (frame, _producer) = frame(false, false);
    let drawing = frame.drawing();
    let pool = fixture::pool();
    let cache = Cache::default();
    let (first, _) = seed(&cache, drawing, &pool);
    let nodes = effects(drawing);
    let range = nodes[0]..source_end(drawing.commands(), nodes[0]).unwrap();
    let root = effect(drawing, nodes[0]);
    let plan = groups(drawing);
    let mut changed = full_grid(root);
    changed.source_scissor = [2, 3, 20, 17];
    assert!(
        probe(
            &cache,
            drawing,
            range.clone(),
            changed,
            &plan,
            &|size| size,
            &pool
        )
        .is_none()
    );
    assert!(
        probe(
            &cache,
            drawing,
            range.clone(),
            full_grid(root),
            &plan,
            &|_| [64, 32],
            &pool
        )
        .is_none()
    );
    let mut middle = effect(drawing, nodes[1]).clone();
    middle.values[0] = EffectValue::Float(0.5f32.to_bits());
    let changed = cache
        .prepare(
            Probe::from_output(first, &middle, full_grid(&middle)),
            &pool,
        )
        .unwrap();
    assert!(cache.publish(changed.observe(), &pool));
    assert!(
        probe(
            &cache,
            drawing,
            range,
            full_grid(root),
            &plan,
            &|size| size,
            &pool
        )
        .is_none()
    );
}

#[test]
fn siblings_different_domains_and_metadata_pressure_do_not_infer_chain_identity() {
    for (extra, domain) in [(true, false), (false, true), (false, false)] {
        let (frame, _producer) = frame(extra, domain);
        let drawing = frame.drawing();
        let pool = fixture::pool();
        let cache = Cache::default();
        seed(&cache, drawing, &pool);
        let nodes = effects(drawing);
        let root = effect(drawing, nodes[0]);
        let refused = reel_core::budget::BudgetPool::new(
            reel_core::budget::BudgetCategory::Temporary,
            reel_core::budget::QueueLimits {
                max_requests: 1,
                max_bytes: 0,
            },
        );
        let metadata = if extra || domain { &pool } else { &refused };
        assert!(
            probe(
                &cache,
                drawing,
                nodes[0]..source_end(drawing.commands(), nodes[0]).unwrap(),
                full_grid(root),
                &groups(drawing),
                &|size| size,
                metadata
            )
            .is_none()
        );
        assert_eq!(refused.usage().bytes, 0);
    }
}
