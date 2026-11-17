//! Finite real-backend checks for optional result lifetimes and physical tickets.
use super::*;
mod composition;
#[cfg(feature = "profiling")]
mod raster_scale;
mod region;
mod resolution;
mod terminal_schedule;
mod uniform_pressure;
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
    transformed_chain(size, Affine::IDENTITY, false)
}
fn transformed_chain(
    size: [u32; 2],
    transform: Affine,
    patterned: bool,
) -> (RenderFrame, PresentationBridge) {
    resolution_chain(size, transform, patterned, 1.)
}
fn resolution_chain(
    size: [u32; 2],
    transform: Affine,
    patterned: bool,
    resolution_scale: f64,
) -> (RenderFrame, PresentationBridge) {
    image_composition_chain(size, transform, patterned, resolution_scale, 1)
}
fn image_composition_chain(
    size: [u32; 2],
    transform: Affine,
    patterned: bool,
    resolution_scale: f64,
    images: usize,
) -> (RenderFrame, PresentationBridge) {
    let (frame, bridge, _) =
        effect_scene(size, transform, patterned, resolution_scale, images, false);
    (frame, bridge)
}

fn effect_scene(
    size: [u32; 2],
    transform: Affine,
    patterned: bool,
    resolution_scale: f64,
    images: usize,
    sibling: bool,
) -> (
    RenderFrame,
    PresentationBridge,
    Option<reel_ui::core::WidgetId>,
) {
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
    .unwrap()
    .with_source_footprint(
        crate::effect::source_footprint(&program),
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
    let image_size = if patterned { [8, 8] } else { [1, 1] };
    let pixels = if patterned {
        (0u8..64)
            .flat_map(|i| {
                [
                    i.wrapping_mul(29),
                    i.wrapping_mul(43),
                    i.wrapping_mul(71),
                    if i % 3 == 0 { 0 } else { 211 },
                ]
            })
            .collect::<Vec<_>>()
    } else {
        vec![255; 4]
    };
    let image = registry
        .register_image(
            ResourceVersion {
                key: AssetKey("admission-image".into()),
                version: ContentVersion([18; 32]),
            },
            OwnedBytes::new(pixels.clone(), pool.reserve(pixels.len() as u64).unwrap()).unwrap(),
            image_size,
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Linear,
        )
        .unwrap();
    let bounds = Rect::new(0., 0., f64::from(size[0]), f64::from(size[1]));
    let mut scene = Scene::new();
    for image_index in 0..images {
        scene.draw(Draw::Fill {
            transform: Affine::translate((image_index as f64 * 2.25, image_index as f64 * -1.5)),
            fill_rule: Default::default(),
            brush: Brush::Image(ImageBrush::new(crate::test_support::image_data(
                &registry.acquire(image).unwrap(),
            ))),
            brush_transform: None,
            shape: Geometry::Rect(bounds),
            composite: Composite::default(),
        });
    }
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
        resolution_scale,
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
    let mut sibling_id = None;
    if sibling {
        let mut overlay = Scene::new();
        overlay.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Solid(Color::WHITE),
            brush_transform: None,
            shape: Geometry::Rect(Rect::new(0., 0., 8., 8.)),
            composite: Default::default(),
        });
        let overlay = r01_fixture::RecordWidget::new(overlay).prepare();
        sibling_id = Some(overlay.id());
        let mut root = r01_fixture::RecordWidget::new(Scene::new());
        root.clip = false;
        root.inner.add(
            widget.prepare(),
            Point::ZERO,
            Size::new(bounds.width(), bounds.height()),
        );
        root.inner
            .add(overlay, Point::new(16., 16.), Size::new(8., 8.));
        widget = root;
    }
    let mut bridge = r01_fixture::widget_bridge(widget, transform);
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
    (build.finish().unwrap(), bridge, sibling_id)
}
fn renderer(limit: u64) -> Renderer {
    Renderer::new(
        pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            crate::gpu::DeviceGeneration(1),
            crate::gpu::GpuLimits {
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
#[ignore = "actual GPU effect-cache passes and sibling transition pixels"]
fn independent_transition_keeps_static_effect_source_cached() {
    use reel_ui::frame::{FrameTransition, RetainBuild};
    use reel_ui::resources::ResourceSnapshot;
    let (original, mut bridge, sibling) =
        effect_scene([64, 48], Affine::IDENTITY, true, 1., 1, true);
    let pool = r01_fixture::pool();
    let mut retained = RetainBuild::new(&original, sibling, r01_fixture::limits(), &pool).unwrap();
    while !retained.step(r01_fixture::step_budget()).unwrap().complete {}
    let mut build = FrameBuild::new(
        bridge.record(FrameId(2)).unwrap(),
        ResourceSnapshot::capture(original.resources().leases().iter().cloned().map(Ok), &pool)
            .unwrap(),
        original.info(),
        r01_fixture::limits(),
        &pool,
    )
    .unwrap();
    build
        .set_transitions(vec![FrameTransition {
            root: sibling,
            previous: retained.finish().unwrap(),
            progress: 0.5,
            mask: None,
        }])
        .unwrap();
    while !build.step(r01_fixture::step_budget()).unwrap().complete {}
    let next = build.finish().unwrap();

    let prepare = |renderer: &mut Renderer| {
        for _ in 0..32 {
            match renderer.prepare(&next) {
                Err(GpuError::Preparing) => {
                    renderer
                        .gpu
                        .device
                        .poll(wgpu::PollType::wait_indefinitely())
                        .unwrap();
                    renderer.poll().unwrap();
                }
                result => return result.unwrap(),
            }
        }
        panic!("transition did not prepare");
    };
    let mut subject = renderer(64 << 20);
    let initial = subject.prepare(&original).unwrap();
    subject.readback(initial).unwrap();
    let first = prepare(&mut subject);
    subject.readback(first).unwrap();
    let warm = prepare(&mut subject);
    let mut control = renderer(64 << 20);
    let cold = prepare(&mut control);
    assert!(!warm.effect_pins.is_empty());
    same_pixels(
        &control.readback(cold).unwrap(),
        &subject.readback(warm).unwrap(),
    );
}

#[test]
#[ignore = "finite actual GPU local-grid versus legacy-coordinate pixel comparison"]
fn same_domain_local_coordinates_preserve_moving_scaled_rotated_chain_pixels() {
    use crate::common::effect_plan::{roi::testing::collect, testing::with_legacy};
    let mut control = renderer(64 << 20);
    let mut subject = renderer(64 << 20);
    for (x, y, angle, sx, sy) in [
        (0., 0., 0., 1., 1.),
        (0.125, -0.375, 0., 0.837, 1.213),
        (-3.7, 5.3, 0.37, 0.837, 1.213),
        (9.7, -7.3, -0.82, 1.117, 0.763),
        (-0.5, 0.5, 0.31, -0.913, 1.071),
        (31.25, -13.75, 1.21, 0.607, 1.313),
    ] {
        let transform = Affine::translate((32. + x, 24. + y))
            * Affine::rotate(angle)
            * Affine::scale_non_uniform(sx, sy)
            * Affine::translate((-32., -24.));
        let (frame, _recording) = transformed_chain([64, 48], transform, true);
        // Both executions draw every existing materialized node. Cache reuse is
        // tested separately; this oracle changes only the coordinate calculation.
        control.effect_results.clear();
        subject.effect_results.clear();
        let (reference, old_passes) = with_legacy(|| collect(|| control.prepare(&frame).unwrap()));
        let quality = reference.quality();
        let expected = control.readback(reference).unwrap();
        let (prepared, passes) = collect(|| subject.prepare(&frame).unwrap());
        assert_eq!(prepared.quality(), quality);
        assert_eq!(old_passes.len(), passes.len());
        assert!(
            passes.len() >= 6,
            "the shared-grid materialized chain must execute"
        );
        let actual = subject.readback(prepared).unwrap();
        let changed = expected.iter().zip(&actual).filter(|(a, b)| a != b).count();
        assert_eq!(changed, 0, "local raster pixels changed for {transform:?}");
        let (warm, inputs) =
            crate::common::effect_inputs::testing::collect(|| subject.prepare(&frame).unwrap());
        if angle == 0.37 {
            assert!(
                inputs.iter().any(|input| input.inputs > 0),
                "the rotated noninteger source grid must use prepared inputs"
            );
        }
        assert_eq!(
            subject.readback(warm).unwrap(),
            expected,
            "prepared source pixels changed for {transform:?}"
        );
    }
}

#[test]
#[ignore = "actual GPU frozen source-input lifetimes, pixels and exact-budget admission"]
fn prepared_effect_inputs_defer_ancestors_and_replace_exact_budget_scratch() {
    use crate::common::{
        effect_inputs::testing::collect,
        effect_result::{Grid, Probe},
    };
    use reel_ui::frame::FrameOp;
    let (frame, _recording) = transformed_chain([64, 48], Affine::IDENTITY, true);
    let mut subject = renderer(64 << 20);
    let initial = subject.prepare(&frame).unwrap();
    let quality = initial.quality();
    let first_pixels = initial.effect_pins.first().unwrap().clone();
    let first = crate::gpu::retained::Output {
        allocation: first_pixels.allocation.as_ref().unwrap().upgrade().unwrap(),
        target: first_pixels,
    };
    let expected = subject.readback(initial).unwrap();
    let nodes: Vec<_> = frame
        .commands()
        .iter()
        .enumerate()
        .filter_map(|(index, command)| {
            matches!(&command.op, FrameOp::PushGroup(group) if group.effect.is_some())
                .then_some(index)
        })
        .collect();
    let groups = crate::common::scopes::GroupPlan::new(
        frame.commands(),
        Some(0),
        std::iter::empty(),
        std::iter::empty(),
        |_| Ok(()),
    )
    .unwrap();
    let grid = Grid {
        source_transform: Affine::IDENTITY.as_coeffs().map(f64::to_bits),
        effect_transform: Affine::IDENTITY.as_coeffs().map(f64::to_bits),
        effect_context: None,
        source_size: [64, 48],
        source_storage: [64, 48],
        source_scissor: [0, 0, 64, 48],
        output_size: [64, 48],
        output_storage: [64, 48],
        scissor: [0, 0, 64, 48],
        quality: [
            u32::from(quality.format == wgpu::TextureFormat::Rgba16Float),
            quality.samples,
        ],
    };
    let mut preceding = None;
    let mut last_input = None;
    let mut first_token = None;
    for &push in nodes.iter().rev() {
        let FrameOp::PushGroup(group) = &frame.commands()[push].op else {
            unreachable!()
        };
        let effect = group.effect.as_ref().unwrap();
        let grid = grid.color_context(effect);
        let probe = preceding.map_or_else(
            || {
                Probe::new(
                    frame.drawing(),
                    push..crate::common::effect_result::source_end(frame.commands(), push).unwrap(),
                    effect,
                    grid,
                    true,
                )
                .unwrap()
            },
            |token| Probe::from_output(token, effect, grid),
        );
        let token = subject.effect_results.lookup_content(&probe).unwrap();
        first_token.get_or_insert(token);
        if groups.is_source_child(push)
            && let Some((_, output)) = subject.effect_results.lookup(&probe)
        {
            last_input = Some((preceding, push, token, output));
        }
        preceding = Some(token);
    }
    assert_eq!(first.target.effect_token(), first_token);
    let (tail_source, tail_push, tail_token, tail) = last_input.unwrap();
    subject.effect_results.release_outputs();
    let first_push = *nodes.last().unwrap();
    let FrameOp::PushGroup(first_group) = &frame.commands()[first_push].op else {
        unreachable!()
    };
    let first_effect = first_group.effect.as_ref().unwrap();
    let raw = Probe::new(
        frame.drawing(),
        first_push..crate::common::effect_result::source_end(frame.commands(), first_push).unwrap(),
        first_effect,
        grid.color_context(first_effect),
        true,
    )
    .unwrap();
    let seed = subject
        .effect_results
        .prepare(raw, &subject.budgets.transient)
        .unwrap();
    assert_eq!(seed.token, first_token.unwrap());
    assert!(
        subject
            .effect_results
            .publish(seed.finish(first), &subject.budgets.transient)
    );
    let (prepared, traces) = collect(|| subject.prepare(&frame).unwrap());
    assert!(
        traces.iter().any(|trace| trace.inputs > 0),
        "the oldest source must replace a deep producer path"
    );
    for trace in &traces {
        assert!(trace.stats.recounts <= trace.stats.candidates + 1);
        assert!(trace.stats.events > 0 && trace.stats.scans > 0);
        eprintln!(
            "effect input deep scans={} candidates={} events={} recounts={} selected={} scan_ns={} recount_ns={}",
            trace.stats.scans,
            trace.stats.candidates,
            trace.stats.events,
            trace.stats.recounts,
            trace.inputs,
            trace.stats.scan_ns,
            trace.stats.recount_ns
        );
    }
    assert_eq!(subject.readback(prepared).unwrap(), expected);

    // Restore only the last early-reusable materialization. The complete
    // original scratch fits exactly; adding its pixels without pruning cannot.
    subject.effect_results.release_outputs();
    let FrameOp::PushGroup(tail_group) = &frame.commands()[tail_push].op else {
        unreachable!()
    };
    let tail_effect = tail_group.effect.as_ref().unwrap();
    let seed = subject
        .effect_results
        .prepare(
            Probe::from_output(
                tail_source.unwrap(),
                tail_effect,
                grid.color_context(tail_effect),
            ),
            &subject.budgets.transient,
        )
        .unwrap();
    assert_eq!(seed.token, tail_token);
    assert!(
        subject
            .effect_results
            .publish(seed.finish(tail), &subject.budgets.transient)
    );
    subject.targets.clear();
    eprintln!(
        "effect input old uniform owners resident={}",
        subject.usage().targets
    );
    subject.effect_uniforms.clear();
    eprintln!(
        "effect input retired scratch resident={}",
        subject.usage().targets
    );
    let (_, required, _) = crate::common::offscreen::Plan::choose(
        &frame,
        None,
        [64, 48],
        false,
        quality.samples,
        subject.gpu.device.limits().max_texture_dimension_2d,
        &Default::default(),
        64 << 20,
        &|size| crate::gpu::targets::target_bytes(size, quality),
        crate::common::target_plan::MaskExecution::Inline,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    subject.gpu.limits.target_bytes = required;
    let (prepared, traces) = collect(|| subject.prepare(&frame).unwrap());
    assert_eq!(prepared.quality(), quality);
    assert!(
        traces
            .iter()
            .any(|trace| trace.inputs > 0 && trace.pins > 0 && trace.bytes <= required)
    );
    assert!(prepared.budget_scope.targets.usage().bytes <= required);
    eprintln!(
        "effect input exact required={required} local={} resident={} pins={} targets={} held={}",
        prepared.budget_scope.targets.usage().bytes,
        subject.usage().targets,
        prepared.effect_pins.len(),
        prepared.targets.len(),
        prepared.retained_inputs.len()
    );
    assert!(
        subject.usage().targets <= required,
        "physical owners must fit after old scratch retirement"
    );
    assert_eq!(subject.readback(prepared).unwrap(), expected);
}

#[test]
#[ignore = "actual GPU cold exact-budget bootstrap, cancellation and pixel comparison"]
fn sealed_effect_tail_bootstraps_without_an_extra_physical_owner() {
    let (frame, _recording) = transformed_chain([64, 48], Affine::IDENTITY, true);
    let mut gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        crate::gpu::DeviceGeneration(1),
        crate::gpu::GpuLimits::default(),
    ))
    .unwrap();
    let quality = gpu.quality([64, 48]).unwrap();
    let (_, required, _) = crate::common::offscreen::Plan::choose(
        &frame,
        None,
        [64, 48],
        false,
        quality.samples,
        gpu.device.limits().max_texture_dimension_2d,
        &Default::default(),
        gpu.limits.target_bytes,
        &|size| crate::gpu::targets::target_bytes(size, quality),
        crate::common::target_plan::MaskExecution::Inline,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    gpu.limits.target_bytes = required;
    let mut subject = Renderer::new(gpu);
    let cancelled = subject.prepare(&frame).unwrap();
    assert_eq!(cancelled.quality(), quality);
    assert!(
        !cancelled.effect_pins.is_empty(),
        "the final ordinary writer must transfer without spare storage"
    );
    assert!(
        !subject.effect_results.has_outputs(),
        "sealed private pixels wait for submission"
    );
    drop(cancelled);
    assert!(!subject.effect_results.has_outputs());
    let (cold, cold_passes, _) =
        crate::gpu::targets::pass_trace::collect_counts(|| subject.prepare(&frame).unwrap());
    assert_eq!(cold.quality(), quality);
    assert!(cold.budget_scope.targets.usage().bytes <= required);
    assert!(subject.usage().targets <= required);
    let owners: Vec<_> = cold.targets.iter().map(Arc::downgrade).collect();
    let expected = subject.readback(cold).unwrap();
    assert!(
        subject.effect_results.has_outputs(),
        "submission publishes the surviving materialization"
    );
    let ((warm, warm_passes, _), inputs) = crate::common::effect_inputs::testing::collect(|| {
        crate::gpu::targets::pass_trace::collect_counts(|| subject.prepare(&frame).unwrap())
    });
    assert_eq!(warm.quality(), quality);
    assert!(inputs.iter().any(|input| input.inputs > 0));
    assert!(
        warm_passes < cold_passes,
        "the first cached input must remove producer passes"
    );
    let local = warm.budget_scope.targets.usage().bytes;
    let resident = subject.usage().targets;
    assert!(local <= required && resident <= required);
    eprintln!(
        "effect tail bootstrap budget={required} cold_passes={cold_passes} warm_passes={warm_passes} local={local} resident={resident} cached_inputs={}",
        inputs.iter().map(|input| input.inputs).sum::<usize>()
    );
    assert_eq!(subject.readback(warm).unwrap(), expected);
    // A different recorded producer cannot select the previous tail. Its exact
    // same-budget frame must reuse those owners before any pressure retry.
    let (next, _next_recording) = transformed_chain([64, 48], Affine::IDENTITY, true);
    let (replacement, inputs) =
        crate::common::effect_inputs::testing::collect(|| subject.prepare(&next).unwrap());
    assert!(inputs.iter().all(|input| input.inputs == 0));
    assert!(replacement.retained_inputs.is_empty());
    assert_eq!(replacement.quality(), quality);
    assert!(replacement.targets.iter().all(|target| {
        owners
            .iter()
            .any(|owner| owner.ptr_eq(&Arc::downgrade(target)))
    }));
    assert!(subject.usage().targets <= required);
    assert_eq!(subject.readback(replacement).unwrap(), expected);
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
        control.effect_results.test_stats().0 > 0,
        "the control must retain a materialized consumer"
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
        &|size| crate::gpu::targets::target_bytes(size, quality),
        crate::common::target_plan::MaskExecution::Inline,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    let one = crate::gpu::targets::target_bytes([64, 48], quality).unwrap();
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
            assert!(bounded.effect_results.test_stats().0 > 0);
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
                    0,
                    "retired owners must be reclaimed without replaying the frame"
                );
            }
            assert_eq!(
                prepared.quality(),
                quality,
                "optional caching cannot lower quality"
            );
            let local = prepared.budget_scope.targets.usage().bytes;
            assert!(local <= limit);
            assert!(bounded.usage().targets <= limit);
            let mut physical = Vec::new();
            for owner in prepared.targets.iter().chain(&prepared.retained_inputs) {
                if !physical.iter().any(|old| Arc::ptr_eq(old, owner)) {
                    physical.push(owner.clone());
                }
            }
            let actual: u64 = physical
                .iter()
                .map(|owner| {
                    crate::gpu::targets::target_bytes(
                        owner.target.storage_size(),
                        prepared.quality(),
                    )
                    .unwrap()
                })
                .sum();
            assert_eq!(
                local, actual,
                "each admitted physical owner is charged once"
            );
            eprintln!(
                "effect admission required={required} slack={slack} local={local} resident={} pins={} attempt={attempt}",
                bounded.usage().targets,
                prepared.effect_pins.len()
            );
            same_pixels(&expected, &bounded.readback(prepared).unwrap());
            eprintln!(
                "effect bootstrap slack={slack} submitted_pixel_outputs={}",
                bounded.effect_results.test_stats().0
            );
        }
    }
    // Both sides read the same immutable chain, so each cached physical output
    // must have only one local reservation and one frame pin.
    let prepared = control.crossfade(&frame, &frame, 0.5).unwrap();
    let mut owners = Vec::new();
    for pin in &prepared.effect_pins {
        assert!(
            !owners
                .iter()
                .any(|owner: &&crate::gpu::targets::Target| owner.same_allocation(pin)),
            "a physical cache hit was retained twice"
        );
        owners.push(pin);
    }
    assert!(
        !owners.is_empty(),
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
            crate::gpu::targets::target_bytes(allocation.target.storage_size(), quality).unwrap()
        })
        .sum();
    assert_eq!(prepared.budget_scope.targets.usage().bytes, bytes);
    eprintln!(
        "effect repeated reads physical_outputs={} local={bytes}",
        owners.len()
    );
    same_pixels(&expected, &control.readback(prepared).unwrap());
}

#[test]
#[ignore = "finite actual GPU animation, stable-cache recovery and numeric comparison"]
fn animated_effects_recycle_pixels_and_resume_retention_when_settled() {
    use reel_ui::{frame::FrameOp, resources::ResourceSnapshot};
    let (original, mut bridge) = chain([64, 48]);
    let owner = original
        .commands()
        .iter()
        .rev()
        .find_map(|command| {
            if let FrameOp::PushGroup(group) = &command.op {
                group.effect.as_ref().map(|_| group.owner)
            } else {
                None
            }
        })
        .unwrap();
    let pool = r01_fixture::pool();
    let mut subject = renderer(64 << 20);
    let mut control = renderer(64 << 20);
    let warm = subject.prepare(&original).unwrap();
    subject.readback(warm).unwrap();
    let mut settled = None;
    let mut expected = Vec::new();
    let mut cold_passes = 0;
    for index in 1..=8 {
        bridge.edit_widget(owner, |mut widget| {
            let mut widget = widget.downcast::<r01_fixture::RecordWidget>();
            let effect = widget.widget.effect.as_mut().unwrap();
            effect.instance = EffectInstance::new(
                [17; 32],
                effect.instance.program().clone(),
                vec![EffectValue::Vec4([
                    (0.3 + index as f32 * 0.04).to_bits(),
                    0.8f32.to_bits(),
                    0.7f32.to_bits(),
                    1f32.to_bits(),
                ])],
            )
            .unwrap();
            widget.ctx.request_pre_paint();
        });
        let mut build = FrameBuild::new(
            bridge.record(FrameId(index + 1)).unwrap(),
            ResourceSnapshot::capture(original.resources().leases().iter().cloned().map(Ok), &pool)
                .unwrap(),
            original.info(),
            r01_fixture::limits(),
            &pool,
        )
        .unwrap();
        while !build.step(r01_fixture::step_budget()).unwrap().complete {}
        let frame = build.finish().unwrap();
        control.effect_results.clear();
        let (reference, passes, _) =
            crate::gpu::targets::pass_trace::collect_counts(|| control.prepare(&frame).unwrap());
        cold_passes = passes;
        let quality = reference.quality();
        expected = control.readback(reference).unwrap();
        let prepared = subject.prepare(&frame).unwrap();
        assert_eq!(prepared.quality(), quality);
        assert!(
            prepared.effect_pins.is_empty(),
            "changing dependencies must keep scratch pixels reusable"
        );
        assert!(
            !prepared.effect_pending.is_empty(),
            "the observed producer must publish metadata"
        );
        same_pixels(&expected, &subject.readback(prepared).unwrap());
        settled = Some(frame);
    }
    let frame = settled.unwrap();
    let mut pins = 0;
    let mut warm_passes = 0;
    for _ in 0..8 {
        let (prepared, passes, _) =
            crate::gpu::targets::pass_trace::collect_counts(|| subject.prepare(&frame).unwrap());
        warm_passes = passes;
        pins = prepared.effect_pins.len();
        same_pixels(&expected, &subject.readback(prepared).unwrap());
    }
    assert!(
        pins > 0,
        "the settled materialized chain must regain cache reuse"
    );
    let (_, hits) = subject.effect_results.test_stats();
    assert!(hits.iter().sum::<usize>() > 0);
    assert!(
        warm_passes < cold_passes,
        "settled chain must avoid replay: {cold_passes} -> {warm_passes}"
    );
    assert_eq!(warm_passes, 1, "only the terminal consumer must execute");
}
