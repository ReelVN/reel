//! Recorded effect-chain replay costs. No pixels are read or visually accepted.
use crate::{
    effect::*,
    gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer},
    test_support::r01_fixture,
};
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
};
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::PresentationBridge,
    core::{DefaultProperties, PaintEffect, Widget},
    dpi::PhysicalSize,
    frame::{FrameBuild, FrameInfo, RenderFrame},
    imaging::{
        Composite,
        record::{Draw, Geometry, Scene},
    },
    kurbo::{Affine, Point, Rect, Size},
    peniko::{Brush, ImageAlphaType, ImageBrush, ImageFormat},
    resources::{ColorEncoding, EffectShaderDescriptor, EffectShaderFormat, ResourceRegistry},
    stage::effect::*,
};
use std::sync::Arc;

fn node(nodes: &mut Vec<EffectNode>, ty: EffectType, op: EffectOp) -> EffectNodeId {
    let id = EffectNodeId(nodes.len() as u32);
    nodes.push(EffectNode { ty, op });
    id
}
fn kernel(axis: u8, radius: i32) -> Arc<EffectProgram> {
    let mut nodes = Vec::new();
    let uv = node(
        &mut nodes,
        EffectType::Vec2,
        EffectOp::Input(EffectInput::Uv),
    );
    let size = node(
        &mut nodes,
        EffectType::Vec2,
        EffectOp::Input(EffectInput::Size),
    );
    let xy = [0, 1].map(|lane| {
        node(
            &mut nodes,
            EffectType::Float,
            EffectOp::Extract { value: uv, lane },
        )
    });
    let denominator = node(
        &mut nodes,
        EffectType::Float,
        EffectOp::Extract {
            value: size,
            lane: axis,
        },
    );
    let mut sum = node(
        &mut nodes,
        EffectType::Vec4,
        EffectOp::Constant(EffectValue::Vec4([0; 4])),
    );
    for tap in -radius..=radius {
        let offset = node(
            &mut nodes,
            EffectType::Float,
            EffectOp::Constant(EffectValue::Float((tap as f32).to_bits())),
        );
        let offset = node(
            &mut nodes,
            EffectType::Float,
            EffectOp::Binary {
                op: EffectBinary::Divide,
                left: offset,
                right: denominator,
            },
        );
        let coordinate = node(
            &mut nodes,
            EffectType::Float,
            EffectOp::Binary {
                op: EffectBinary::Add,
                left: xy[axis as usize],
                right: offset,
            },
        );
        let mut at = xy;
        at[axis as usize] = coordinate;
        let at = node(&mut nodes, EffectType::Vec2, EffectOp::Vec2(at));
        let sample = node(
            &mut nodes,
            EffectType::Vec4,
            EffectOp::SourceSample { uv: at },
        );
        sum = node(
            &mut nodes,
            EffectType::Vec4,
            EffectOp::Binary {
                op: EffectBinary::Add,
                left: sum,
                right: sample,
            },
        );
    }
    let denominator = node(
        &mut nodes,
        EffectType::Vec4,
        EffectOp::Constant(EffectValue::Vec4([((radius * 2 + 1) as f32).to_bits(); 4])),
    );
    let output = node(
        &mut nodes,
        EffectType::Vec4,
        EffectOp::Binary {
            op: EffectBinary::Divide,
            left: sum,
            right: denominator,
        },
    );
    EffectProgram::new(Vec::new(), nodes, output).unwrap()
}
fn chain(proved: bool) -> (RenderFrame, PresentationBridge) {
    let pool = r01_fixture::pool();
    let mut registry = ResourceRegistry::new(8, 4 << 20).unwrap();
    let version = |id| ResourceVersion {
        key: AssetKey(format!("roi-{id}")),
        version: ContentVersion([id; 32]),
    };
    let image = registry
        .register_image(
            version(20),
            OwnedBytes::new(vec![255; 4], pool.reserve(4).unwrap()).unwrap(),
            [1, 1],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Linear,
        )
        .unwrap();
    let mut scene = Scene::new();
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Image(ImageBrush::new(crate::test_support::image_data(
            &registry.acquire(image).unwrap(),
        ))),
        brush_transform: None,
        shape: Geometry::Rect(Rect::new(32., 32., 1496., 1496.)),
        composite: Composite::default(),
    });
    let mut widget = r01_fixture::RecordWidget::new(scene);
    widget.clip = false;
    widget.inner.clip = Rect::new(0., 0., 1528., 1528.);
    let mut handles = vec![image];
    for (index, (axis, radius)) in [(0, 30), (1, 30), (0, 1), (1, 1)].into_iter().enumerate() {
        let program = kernel(axis, radius);
        let footprint = source_footprint(&program).unwrap();
        let fragment = compile_fragment(&program, Language::Wgsl, 1 << 20).unwrap();
        let id = index as u8 + 1;
        let descriptor = EffectShaderDescriptor::with_textures(
            [id; 32],
            EffectShaderFormat::Wgsl,
            Vec::new(),
            0,
            true,
            EffectType::Vec4,
            fragment.source.as_bytes(),
        )
        .unwrap()
        .with_source_footprint(proved.then_some(footprint), fragment.source.as_bytes())
        .unwrap();
        let shader = registry
            .register_effect_shader(
                version(id),
                descriptor,
                OwnedBytes::new(
                    fragment.source.as_bytes().to_vec(),
                    pool.reserve(fragment.source.len() as u64).unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
        handles.push(shader);
        let mut parent = r01_fixture::RecordWidget::new(Scene::new());
        parent.clip = false;
        parent.inner.clip = Rect::new(0., 0., 1528., 1528.);
        parent.effect = Some(PaintEffect {
            instance: EffectInstance::new([id; 32], program, Vec::new()).unwrap(),
            shader: registry.acquire(shader).unwrap(),
            images: Vec::new(),
            bounds: Rect::new(0., 0., 1528., 1528.),
            coverage_invert: false,
        });
        parent
            .inner
            .add(widget.prepare(), Point::ZERO, Size::new(1528., 1528.));
        widget = parent;
    }
    // Exact first rune pose; production Canvas supplies the aspect-preserving fit.
    let transform = Affine::translate((1437., -53.))
        * Affine::scale_non_uniform(2., 0.65)
        * Affine::translate((-764., -764.));
    let mut overlay = r01_fixture::RecordWidget::new(Scene::new());
    overlay.clip = false;
    overlay.inner.clip = Rect::new(0., 0., 1528., 1528.);
    overlay.blend = reel_ui::peniko::Mix::Overlay.into();
    overlay
        .inner
        .add(widget.prepare(), Point::ZERO, Size::new(1528., 1528.));
    let mut stage = r01_fixture::RecordWidget::new(Scene::new());
    stage.clip = true;
    stage.inner.clip = Rect::new(0., 0., 1024., 576.);
    stage.inner.add(
        overlay.prepare().with_transform(transform),
        Point::ZERO,
        Size::new(1528., 1528.),
    );
    let root = RenderRoot::new(
        reel_ui::widgets::Canvas::new(stage.prepare(), [1024, 576]).prepare(),
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(960, 544),
            scale_factor: 1.,
            default_font: None,
        },
    );
    let mut bridge = r01_fixture::bridge(root);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&handles, &pool).unwrap(),
        FrameInfo {
            logical_size: [960., 544.],
            physical_size: [960, 544],
        },
        r01_fixture::limits(),
        &pool,
    )
    .unwrap();
    while !build.step(r01_fixture::step_budget()).unwrap().complete {}
    (build.finish().unwrap(), bridge)
}

#[test]
#[ignore = "actual adapter replay and scissor costs; no submission or pixel readback"]
fn recorded_large_box_chain_reduces_real_effect_draw_scissors_without_changing_targets() {
    let mut cases = Vec::new();
    for proved in [false, true] {
        let (frame, _recording) = chain(proved);
        let gpu = pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            DeviceGeneration(1),
            GpuLimits {
                target_bytes: 64 << 20,
                ..Default::default()
            },
        ))
        .unwrap();
        let renderer = Renderer::new(gpu);
        let (prepared, draws) =
            crate::common::effect_plan::roi::testing::collect(|| renderer.prepare(&frame).unwrap());
        assert_eq!(draws.len(), 4);
        let areas: Vec<_> = draws.iter().map(|(id, size, scissor)| {
            let area = u64::from(scissor.width) * u64::from(scissor.height);
            eprintln!("recorded ROI proved={proved} pass={} viewport={size:?} scissor={scissor:?} pixels={area}", id[0]);
            area
        }).collect();
        cases.push((prepared.quality(), renderer.usage().targets, areas));
    }
    assert_eq!(cases[0].0, cases[1].0, "unchanged quality");
    assert_eq!(
        cases[0].1, cases[1].1,
        "unchanged physical target allocations"
    );
    assert!(
        cases[1].2[0] < cases[0].2[0] / 2,
        "H61 must reduce actual submitted draw scissor"
    );
    assert!(
        cases[1].2[1] < cases[0].2[1] / 2,
        "V61 must reduce actual submitted draw scissor"
    );
}
