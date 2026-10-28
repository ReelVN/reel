//! Finite real-backend checks for optional result lifetimes and physical tickets.
use super::*;
use crate::test_support::r01_fixture;
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
};
use reel_ui::{
    bridge::PresentationBridge,
    core::{PaintEffect, Widget},
    frame::{FrameBuild, FrameInfo},
    imaging::{
        Composite,
        record::{Draw, Geometry, Scene},
    },
    kurbo::{Point, Rect, Size},
    peniko::{Brush, ImageAlphaType, ImageBrush, ImageFormat},
    resources::{ColorEncoding, EffectShaderDescriptor, EffectShaderFormat, ResourceRegistry},
    stage::effect::*,
};

fn chain(size: [u32; 2]) -> (RenderFrame, PresentationBridge) {
    let pool = r01_fixture::pool();
    let program = EffectProgram::new(
        vec![EffectParameter {
            name: "tint".into(),
            ty: EffectType::Vec4,
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
        ],
        EffectNodeId(3),
    )
    .unwrap();
    let fragment =
        crate::effect::compile_fragment(&program, crate::effect::Language::Wgsl, 16384).unwrap();
    let descriptor = EffectShaderDescriptor::with_textures(
        [17; 32],
        EffectShaderFormat::Wgsl,
        vec![EffectType::Vec4],
        0,
        true,
        EffectType::Vec4,
        fragment.source.as_bytes(),
    )
    .unwrap();
    let mut registry = ResourceRegistry::new(4, 16384).unwrap();
    let shader = registry
        .register_effect_shader(
            ResourceVersion {
                key: AssetKey("admission-effect".into()),
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
                key: AssetKey("admission-image".into()),
                version: ContentVersion([18; 32]),
            },
            OwnedBytes::new(vec![255; 4], pool.reserve(4).unwrap()).unwrap(),
            [1, 1],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Linear,
        )
        .unwrap();
    let bounds = Rect::new(0., 0., f64::from(size[0]), f64::from(size[1]));
    let mut scene = Scene::new();
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Image(ImageBrush::new(crate::test_support::image_data(
            &registry.acquire(image).unwrap(),
        ))),
        brush_transform: None,
        shape: Geometry::Rect(bounds),
        composite: Composite::default(),
    });
    let paint = PaintEffect {
        instance: EffectInstance::new(
            [17; 32],
            program,
            vec![EffectValue::Vec4([
                0.9f32.to_bits(),
                0.8f32.to_bits(),
                0.7f32.to_bits(),
                1f32.to_bits(),
            ])],
        )
        .unwrap(),
        shader: registry.acquire(shader).unwrap(),
        images: Vec::new(),
        bounds,
        coverage_invert: false,
    };
    let mut widget = r01_fixture::RecordWidget::new(scene);
    widget.clip = false;
    widget.effect = Some(paint.clone());
    for _ in 0..6 {
        let mut parent = r01_fixture::RecordWidget::new(Scene::new());
        parent.clip = false;
        parent.effect = Some(paint.clone());
        parent.inner.add(
            widget.prepare(),
            Point::ZERO,
            Size::new(bounds.width(), bounds.height()),
        );
        widget = parent;
    }
    let mut bridge = r01_fixture::widget_bridge(widget, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[shader, image], &pool).unwrap(),
        FrameInfo {
            logical_size: size.map(f64::from),
            physical_size: size,
        },
        r01_fixture::limits(),
        &pool,
    )
    .unwrap();
    while !build.step(r01_fixture::step_budget()).unwrap().complete {}
    (build.finish().unwrap(), bridge)
}
fn renderer(limit: u64) -> Renderer {
    Renderer::new(
        pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            super::super::DeviceGeneration(1),
            super::super::GpuLimits {
                target_bytes: limit,
                ..Default::default()
            },
        ))
        .unwrap(),
    )
}
fn same_pixels(expected: &[[f32; 4]], actual: &[[f32; 4]]) {
    assert_eq!(expected.len(), actual.len());
    for (a, b) in expected.iter().zip(actual) {
        for (x, y) in a.iter().zip(b) {
            assert!((x - y).abs() <= 0.004);
        }
    }
}

#[test]
#[ignore = "finite actual GPU cache admission and numeric comparison"]
fn optional_effect_outputs_leave_the_original_scratch_peak_and_quality_available() {
    let (frame, _recording) = chain([64, 48]);
    let mut control = renderer(64 << 20);
    let prepared = control.prepare(&frame).unwrap();
    let quality = prepared.quality();
    let expected = control.readback(prepared).unwrap();
    assert!(
        control.effect_results.test_stats().0 >= 6,
        "the control must actually retain the chain"
    );
    let (_, required, _) = crate::common::offscreen::Plan::choose(
        &frame,
        None,
        [64, 48],
        false,
        quality.samples,
        control.gpu.device.limits().max_texture_dimension_2d,
        &Default::default(),
        64 << 20,
        &|size| super::super::targets::target_bytes(size, quality),
        crate::common::target_plan::MaskExecution::Inline,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    let one = super::super::targets::target_bytes([64, 48], quality).unwrap();
    for slack in [0, one] {
        let limit = required + slack;
        let mut bounded = renderer(limit);
        let (small, _small_recording) = chain([32, 24]);
        let mut old_owners = Vec::new();
        let before_retries = bounded
            .gpu
            .upload_creations
            .geometry_retries
            .load(Ordering::Relaxed);
        if slack == 0 {
            let warm = bounded.prepare(&small).unwrap();
            old_owners.extend(warm.targets.iter().map(Arc::downgrade));
            bounded.readback(warm).unwrap();
            assert!(bounded.effect_results.test_stats().0 >= 6);
            assert!(old_owners.iter().any(|old| old.upgrade().is_some()));
            eprintln!(
                "effect old-size cache resident={} before required={required}",
                bounded.usage().targets
            );
        }
        for attempt in 0..2 {
            let prepared = bounded.prepare(&frame).unwrap();
            if slack == 0 {
                assert!(
                    old_owners.iter().all(|old| old.upgrade().is_none()),
                    "pressure must actually release the retired old-size cache"
                );
                assert_eq!(
                    bounded
                        .gpu
                        .upload_creations
                        .geometry_retries
                        .load(Ordering::Relaxed)
                        - before_retries,
                    1,
                    "the physical-pressure retry is bounded"
                );
            }
            assert_eq!(
                prepared.quality(),
                quality,
                "optional caching cannot lower quality"
            );
            assert!(prepared.effect_pending.len() <= usize::from(slack != 0));
            assert!(prepared.effect_pins.len() <= usize::from(slack != 0));
            let local = prepared.budget_scope.targets.usage().bytes;
            assert!(local <= limit);
            eprintln!(
                "effect admission required={required} slack={slack} local={local} resident={} pins={} attempt={attempt}",
                bounded.usage().targets,
                prepared.effect_pins.len()
            );
            same_pixels(&expected, &bounded.readback(prepared).unwrap());
        }
    }
    // Both sides read the same immutable chain, so each cached physical output
    // must have only one local reservation and one frame pin.
    let prepared = control.crossfade(&frame, &frame, 0.5).unwrap();
    let mut tokens = Vec::new();
    for pin in &prepared.effect_pins {
        let token = pin.effect_token().unwrap();
        assert!(
            !tokens.contains(&token),
            "a physical cache hit was retained twice"
        );
        tokens.push(token);
    }
    assert!(
        tokens.len() >= 6,
        "the repeated reads must use the cached chain"
    );
    let mut physical = Vec::new();
    for allocation in prepared.targets.iter().chain(&prepared.retained_inputs) {
        if !physical.iter().any(|old| Arc::ptr_eq(old, allocation)) {
            physical.push(allocation.clone());
        }
    }
    let bytes: u64 = physical
        .iter()
        .map(|allocation| {
            super::super::targets::target_bytes(allocation.target.storage_size(), quality).unwrap()
        })
        .sum();
    assert_eq!(prepared.budget_scope.targets.usage().bytes, bytes);
    eprintln!(
        "effect repeated reads physical_outputs={} local={bytes}",
        tokens.len()
    );
    same_pixels(&expected, &control.readback(prepared).unwrap());
}
