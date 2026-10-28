use super::*;
use crate::{
    common::{glyph_plan, image_plan, offscreen, renderer::Budgets, retained, target_plan},
    test_support::r01_fixture as f,
};
use reel_core::{
    budget::{BudgetPool, OwnedBytes},
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
};
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, VisualLayerKind, WindowSizePolicy},
    core::{DefaultProperties, NewWidget, PaintEffect, Widget},
    dpi::PhysicalSize,
    frame::{
        FrameBuild, FrameDraw, FrameGlyphRun, FrameInfo, FrameTransition, InkBounds, RetainBuild,
    },
    imaging::record::{Command, Draw, Scene},
    kurbo::{Point, Rect, Size},
    peniko::{Blob, ImageAlphaType, ImageFormat},
    resources::{
        ColorEncoding, EffectShaderDescriptor, EffectShaderFormat, ResourceData, ResourceLease,
        ResourceRegistry,
    },
    stage::effect::*,
    widgets::Label,
};
use std::sync::Arc;

fn image(registry: &mut ResourceRegistry, pool: &BudgetPool, key: u8) -> ResourceLease {
    let handle = registry
        .register_image(
            ResourceVersion {
                key: AssetKey(format!("source-plan-{key}")),
                version: ContentVersion([key; 32]),
            },
            OwnedBytes::new(vec![255; 4], pool.reserve(4).unwrap()).unwrap(),
            [1, 1],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    registry.acquire(handle).unwrap()
}
fn paint(
    registry: &mut ResourceRegistry,
    pool: &BudgetPool,
    sampled: bool,
    scalar: bool,
    pattern: Option<ResourceLease>,
) -> PaintEffect {
    let program = EffectProgram::with_textures(
        vec![EffectParameter {
            name: "color".into(),
            ty: EffectType::Vec4,
            default: None,
        }],
        vec![EffectTextureParameter {
            name: "pattern".into(),
        }],
        vec![
            EffectNode {
                ty: EffectType::Vec2,
                op: EffectOp::Input(EffectInput::Uv),
            },
            EffectNode {
                ty: EffectType::Vec4,
                op: if sampled {
                    EffectOp::SourceSample {
                        uv: EffectNodeId(0),
                    }
                } else {
                    EffectOp::TextureSample {
                        binding: 0,
                        channel: EffectTextureChannel::Color,
                        uv: EffectNodeId(0),
                    }
                },
            },
            EffectNode {
                ty: EffectType::Vec4,
                op: EffectOp::Parameter(0),
            },
            EffectNode {
                ty: EffectType::Vec4,
                op: EffectOp::Binary {
                    op: EffectBinary::Multiply,
                    left: EffectNodeId(1),
                    right: EffectNodeId(2),
                },
            },
            EffectNode {
                ty: EffectType::Float,
                op: EffectOp::Constant(EffectValue::Float(0.5f32.to_bits())),
            },
        ],
        EffectNodeId(if scalar { 4 } else { 3 }),
    )
    .unwrap();
    let definition = [u8::from(sampled) + 2 * u8::from(scalar) + 10; 32];
    let fragment =
        crate::effect::compile_fragment(&program, crate::effect::Language::Wgsl, 16384).unwrap();
    let descriptor = EffectShaderDescriptor::with_textures(
        definition,
        EffectShaderFormat::Wgsl,
        vec![EffectType::Vec4],
        1,
        fragment.source_sampler,
        fragment.output_type,
        fragment.source.as_bytes(),
    )
    .unwrap();
    let handle = registry
        .register_effect_shader(
            ResourceVersion {
                key: AssetKey(format!("effect-{sampled}-{scalar}")),
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
    PaintEffect {
        instance: EffectInstance::with_textures(
            definition,
            program,
            vec![EffectValue::Vec4([0.37f32.to_bits(); 4])],
            vec!["pattern".into()],
        )
        .unwrap(),
        shader: registry.acquire(handle).unwrap(),
        images: vec![pattern.unwrap_or_else(|| image(registry, pool, 9))],
        bounds: Rect::new(0., 0., 160., 120.),
        coverage_invert: false,
    }
}
fn glyphs(registry: &mut ResourceRegistry, pool: &BudgetPool) -> (Scene, ResourceLease) {
    let font = Blob::from(
        include_bytes!(
            "../../../../examples/author_arcade/resources/fonts/AtkinsonHyperlegible-Regular.ttf"
        )
        .to_vec(),
    );
    let handle = registry
        .register(
            ResourceVersion {
                key: AssetKey("source-plan-font".into()),
                version: ContentVersion([7; 32]),
            },
            ResourceData::Font(font.clone()),
            pool.reserve(font.len() as u64).unwrap(),
        )
        .unwrap();
    let mut root = RenderRoot::new(
        NewWidget::new(Label::new("unconsumed glyphs")),
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
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
    (scene, registry.acquire(handle).unwrap())
}
fn frame(sampled: bool, scalar: bool, variant: u8) -> RenderFrame {
    let pool = f::pool();
    let mut registry = ResourceRegistry::new(8, 1 << 20).unwrap();
    let selected_paint = paint(&mut registry, &pool, sampled, scalar, None);
    let source = image(&mut registry, &pool, 1);
    let (scene, font) = glyphs(&mut registry, &pool);
    let mut handles = vec![
        selected_paint.shader.handle(),
        selected_paint.images[0].handle(),
        source.handle(),
        font.handle(),
    ];
    let mut widget = f::RecordWidget::new(scene);
    widget.clip = variant == 1;
    widget.opacity = if variant == 6 { 1. } else { 0.37 };
    widget.invert = if variant == 2 { 1. } else { 0. };
    if variant == 3 {
        widget.blend = reel_ui::peniko::Mix::Screen.into();
    }
    widget.capture_included = variant != 4;
    widget.effect = Some(selected_paint);
    widget.resource_draws.push((
        source,
        Rect::new(0., 0., 160., 120.),
        Affine::IDENTITY,
        Default::default(),
    ));
    if variant == 6 {
        let pattern = widget.effect.as_ref().unwrap().images[0].clone();
        let parent_paint = paint(&mut registry, &pool, true, false, Some(pattern));
        handles.push(parent_paint.shader.handle());
        let mut parent = f::RecordWidget::new(Scene::new());
        parent.clip = false;
        parent.effect = Some(parent_paint);
        parent
            .inner
            .add(widget.prepare(), Point::ZERO, Size::new(160., 120.));
        widget = parent;
    }
    let mut bridge = f::widget_bridge(
        widget,
        Affine::translate((13.25, 7.75)) * Affine::scale_non_uniform(0.83, 0.71),
    );
    let source = bridge.record(FrameId(1)).unwrap();
    let snapshot = registry.freeze(&handles, &pool).unwrap();
    if variant != 5 {
        return f::build(source, snapshot, &pool);
    }
    let previous = frame(false, false, 0);
    let mut retain = RetainBuild::new(&previous, None, f::limits(), &pool).unwrap();
    let mut budget = f::step_budget();
    loop {
        let progress = retain.step(budget).unwrap();
        if progress.complete {
            break;
        }
        budget.max_units = budget.max_units.max(progress.minimum_units);
        budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
    }
    let mut build = FrameBuild::new(
        source,
        snapshot,
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        f::limits(),
        &pool,
    )
    .unwrap();
    build
        .set_transitions(vec![FrameTransition {
            root: None,
            previous: retain.finish().unwrap(),
            progress: 0.5,
            mask: None,
        }])
        .unwrap();
    loop {
        let progress = build.step(budget).unwrap();
        if progress.complete {
            break;
        }
        budget.max_units = budget.max_units.max(progress.minimum_units);
        budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
    }
    build.finish().unwrap()
}
fn selection(frame: &RenderFrame, capture: bool) -> retained::Plan {
    retained::Plan::collect(
        std::iter::once(frame),
        [400, 300],
        capture,
        &Budgets::new(Default::default()),
        &Budgets::new(Default::default()),
        32 << 20,
        |_| None,
        |_| Ok(()),
    )
    .unwrap()
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
        run: &FrameGlyphRun,
        _: f64,
        _: RecordLocation,
    ) -> Result<InkBounds, GpuError> {
        self.0 += run.glyphs.len();
        Ok(InkBounds::Unbounded)
    }
}
fn targets(frame: &RenderFrame, selection: &retained::Plan, samples: u32) -> u64 {
    target_plan::Input {
        frame,
        second: None,
        size: [400, 300],
        capture: false,
        samples,
        max_dimension: 4096,
        glyphs: &glyph_plan::Plan::default(),
        schedule: &offscreen::Plan::default(),
        masks: target_plan::MaskExecution::Sampled,
        selection,
        cost: &|size| Ok(u64::from(size[0]) * u64::from(size[1]) * 4),
    }
    .measure(|_| Ok(()))
    .unwrap()
    .bytes
}
#[test]
fn unread_color_source_removes_draw_demand_and_raw_targets_together() {
    let frame = frame(false, false, 0);
    let selected = selection(&frame, false);
    let complete = retained::Plan::default();
    let mut dead_draws = 0;
    let mut effect = None;
    for (index, command) in frame.commands().iter().enumerate() {
        match &command.op {
            FrameOp::Draw(FrameDraw::GlyphRun(_) | FrameDraw::Fill { .. }) => {
                assert!(!selected.selected(frame.drawing(), index));
                dead_draws += 1;
            }
            FrameOp::PushGroup(group) if group.effect.is_some() => {
                assert!(selected.sources.end(frame.drawing(), index).is_some());
                effect = group.effect.as_ref();
            }
            _ => {}
        }
    }
    assert!(dead_draws >= 2);
    let effect = effect.unwrap();
    assert_eq!(effect.values, [EffectValue::Vec4([0.37f32.to_bits(); 4])]);
    let demand =
        image_plan::collect(std::iter::once(&frame), 4096, false, &selected, |_| Ok(())).unwrap();
    assert_eq!(demand.images.len(), 1);
    assert_eq!(demand.images[0].source.handle(), effect.images[0]);
    assert_eq!(
        image_plan::collect(std::iter::once(&frame), 4096, false, &complete, |_| Ok(()))
            .unwrap()
            .images
            .len(),
        2
    );
    for (plan, expected_empty) in [(&selected, true), (&complete, false)] {
        let mut glyphs = Glyphs::default();
        glyph_plan::Plan::collect(
            std::iter::once(&frame),
            Affine::IDENTITY,
            4096,
            false,
            plan,
            &mut glyphs,
        )
        .unwrap();
        assert_eq!(glyphs.0 == 0, expected_empty);
    }
    assert_eq!(targets(&frame, &selected, 1), 400 * 300 * 4);
    for samples in [1, 4] {
        assert!(targets(&frame, &selected, samples) < targets(&frame, &complete, samples));
    }
}
#[test]
fn sampled_scalar_clipped_special_and_capture_dependencies_keep_their_producers() {
    for (sampled, scalar, variant, capture) in [
        (true, false, 0, false),
        (false, true, 0, false),
        (false, false, 1, false),
        (false, false, 2, false),
        (false, false, 3, false),
        (false, false, 4, false),
        (false, false, 5, false),
        (false, false, 0, true),
    ] {
        let frame = frame(sampled, scalar, variant);
        let selected = selection(&frame, capture);
        assert!(selected.sources.entries.is_empty());
        assert!(
            frame
                .commands()
                .iter()
                .enumerate()
                .all(|(index, _)| selected.sources.selected(frame.drawing(), index))
        );
    }
}

#[test]
fn nested_source_transfer_keeps_the_inherited_grid_and_target_high_water() {
    let frame = frame(false, false, 6);
    let selected = selection(&frame, false);
    assert_eq!(selected.sources.entries.len(), 1);
    let entry = selected.sources.entries[0];
    let FrameOp::PushGroup(child) = &frame.commands()[entry.push].op else {
        unreachable!()
    };
    let parent = frame.commands()[..entry.push]
        .iter()
        .find_map(|command| match &command.op {
            FrameOp::PushGroup(group) if group.effect.is_some() => Some(group),
            _ => None,
        })
        .unwrap();
    assert!(effect_plan::same_source_domain(child, Some(parent)));
    let transform = Affine::translate((7.25, -9.5)) * Affine::scale_non_uniform(0.83, 0.71);
    let layout = layout(
        child,
        Some(parent),
        transform,
        [133, 86],
        4096,
        frame.commands()[entry.push].origin.at,
    )
    .unwrap();
    assert_eq!(layout.size, [133, 86]);
    assert_eq!(layout.frame_to_source, transform);
    let proof = crate::common::scopes::GroupPlan::new(
        frame.commands(),
        Some(0),
        std::iter::empty(),
        std::iter::empty(),
        |_| Ok(()),
    )
    .unwrap();
    assert!(proof.is_source_child(entry.push));
    assert!(effect_plan::forwards_source(
        child,
        &layout,
        transform,
        [133, 86]
    ));
    for samples in [1, 4] {
        assert_eq!(
            targets(&frame, &selected, samples),
            targets(&frame, &retained::Plan::default(), samples)
        );
    }
}
