//! Experimental density changes have a separate quality oracle from cache reuse.
use super::*;
use crate::common::effect_plan::{raster_scale::testing::with_enabled, roi::testing::collect};
use reel_ui::resources::{ResourceLease, ResourceSnapshot};

mod archive;

fn blur_program(axis: u8, radius: i32) -> std::sync::Arc<EffectProgram> {
    fn push(nodes: &mut Vec<EffectNode>, ty: EffectType, op: EffectOp) -> EffectNodeId {
        let id = EffectNodeId(nodes.len() as u32);
        nodes.push(EffectNode { ty, op });
        id
    }
    let mut nodes = Vec::new();
    let uv = push(
        &mut nodes,
        EffectType::Vec2,
        EffectOp::Input(EffectInput::Uv),
    );
    let size = push(
        &mut nodes,
        EffectType::Vec2,
        EffectOp::Input(EffectInput::Size),
    );
    let xy = [0, 1].map(|lane| {
        push(
            &mut nodes,
            EffectType::Float,
            EffectOp::Extract { value: uv, lane },
        )
    });
    let denominator = push(
        &mut nodes,
        EffectType::Float,
        EffectOp::Extract {
            value: size,
            lane: axis,
        },
    );
    let mut sum = push(
        &mut nodes,
        EffectType::Vec4,
        EffectOp::Constant(EffectValue::Vec4([0; 4])),
    );
    for tap in -radius..=radius {
        let offset = push(
            &mut nodes,
            EffectType::Float,
            EffectOp::Constant(EffectValue::Float((tap as f32).to_bits())),
        );
        let offset = push(
            &mut nodes,
            EffectType::Float,
            EffectOp::Binary {
                op: EffectBinary::Divide,
                left: offset,
                right: denominator,
            },
        );
        let coordinate = push(
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
        let at = push(&mut nodes, EffectType::Vec2, EffectOp::Vec2(at));
        let sample = push(
            &mut nodes,
            EffectType::Vec4,
            EffectOp::SourceSample { uv: at },
        );
        sum = push(
            &mut nodes,
            EffectType::Vec4,
            EffectOp::Binary {
                op: EffectBinary::Add,
                left: sum,
                right: sample,
            },
        );
    }
    let divisor = push(
        &mut nodes,
        EffectType::Vec4,
        EffectOp::Constant(EffectValue::Vec4([((radius * 2 + 1) as f32).to_bits(); 4])),
    );
    let mean = push(
        &mut nodes,
        EffectType::Vec4,
        EffectOp::Binary {
            op: EffectBinary::Divide,
            left: sum,
            right: divisor,
        },
    );
    EffectProgram::new(Vec::new(), nodes, mean).unwrap()
}

fn blurred_chain(transform: Affine) -> (RenderFrame, PresentationBridge) {
    blurred_chain_radii(transform, [2, 2, 1, 1])
}

fn blurred_chain_radii(transform: Affine, radii: [i32; 4]) -> (RenderFrame, PresentationBridge) {
    let pool = r01_fixture::pool();
    let mut registry = ResourceRegistry::new(8, 1 << 20).unwrap();
    let version = |id| ResourceVersion {
        key: AssetKey(format!("raster-blur-{id}")),
        version: ContentVersion([id; 32]),
    };
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
            version(20),
            OwnedBytes::new(pixels, pool.reserve(256).unwrap()).unwrap(),
            [8, 8],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Linear,
        )
        .unwrap();
    let bounds = Rect::new(0., 0., 64., 48.);
    let mut scene = Scene::new();
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Image(ImageBrush::new(crate::test_support::image_data(
            &registry.acquire(image).unwrap(),
        ))),
        // Painter takes a brush-to-local map; FrameBuild stores its inverse.
        brush_transform: Some(Affine::scale_non_uniform(64. / 8., 48. / 8.)),
        shape: Geometry::Rect(Rect::new(2., 2., 62., 46.)),
        composite: Composite {
            alpha: 0.625,
            ..Default::default()
        },
    });
    let mut widget = r01_fixture::RecordWidget::new(scene);
    widget.clip = false;
    build_blur_chain(
        widget,
        vec![registry.acquire(image).unwrap()],
        bounds,
        FrameInfo {
            logical_size: [64., 48.],
            physical_size: [64, 48],
        },
        transform,
        radii,
    )
}

fn build_blur_chain(
    mut widget: r01_fixture::RecordWidget,
    mut resources: Vec<ResourceLease>,
    bounds: Rect,
    info: FrameInfo,
    transform: Affine,
    radii: [i32; 4],
) -> (RenderFrame, PresentationBridge) {
    let pool = r01_fixture::pool();
    let mut registry = ResourceRegistry::new(4, 1 << 20).unwrap();
    for (index, radius) in radii.into_iter().enumerate() {
        let axis = (index % 2) as u8;
        let id = index as u8 + 1;
        let program = blur_program(axis, radius);
        let fragment =
            crate::effect::compile_fragment(&program, crate::effect::Language::Wgsl, 1 << 20)
                .unwrap();
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
        .with_source_footprint(
            crate::effect::source_footprint(&program),
            fragment.source.as_bytes(),
        )
        .unwrap();
        let shader = registry
            .register_effect_shader(
                ResourceVersion {
                    key: AssetKey(format!("raster-blur-{id}")),
                    version: ContentVersion([id; 32]),
                },
                descriptor,
                OwnedBytes::new(
                    fragment.source.as_bytes().to_vec(),
                    pool.reserve(fragment.source.len() as u64).unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
        resources.push(registry.acquire(shader).unwrap());
        let mut parent = r01_fixture::RecordWidget::new(Scene::new());
        parent.clip = false;
        parent.effect = Some(PaintEffect {
            instance: EffectInstance::new([id; 32], program, Vec::new()).unwrap(),
            shader: registry.acquire(shader).unwrap(),
            images: Vec::new(),
            bounds,
            coverage_invert: false,
            resolution_scale: 1.,
        });
        parent
            .inner
            .add(widget.prepare(), Point::ZERO, bounds.size());
        widget = parent;
    }
    let mut bridge = r01_fixture::widget_bridge(widget, transform);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        ResourceSnapshot::capture(resources.into_iter().map(Ok), &pool).unwrap(),
        info,
        r01_fixture::limits(),
        &pool,
    )
    .unwrap();
    while !build.step(r01_fixture::step_budget()).unwrap().complete {}
    (build.finish().unwrap(), bridge)
}

#[test]
#[ignore = "actual GPU rotating local-raster experiment: cache hits and fresh-pixel oracle"]
fn rotation_invariant_raster_measures_cache_reuse_without_relaxing_geometry() {
    let (original, bridge) = blurred_chain_radii(Affine::IDENTITY, [30, 30, 1, 1]);
    let transforms = [0., 0., 0.11, 0.37, 0.82, -0.26, -0.82].map(|angle| {
        Affine::translate((32., 24.))
            * Affine::scale_non_uniform(1.875, 0.609375)
            * Affine::rotate(angle)
            * Affine::translate((-32., -24.))
    });
    check_rotation_grid(original, bridge, &transforms, 64 << 20, true, None);
}

#[test]
#[ignore = "actual GPU rotation demand, unchanged pixels and cached repeated poses"]
fn submitted_rotation_preserves_the_demand_grid_and_reuses_repeated_poses() {
    let transforms = [0., 0., 0.11, 0.37, 0.37, 0.37, 0.82, -0.26, -0.26, -0.26].map(|angle| {
        Affine::translate((32., 24.))
            * Affine::scale_non_uniform(1.875, 0.609375)
            * Affine::rotate(angle)
            * Affine::translate((-32., -24.))
    });
    for limit in [64 << 20, 90_000] {
        let (frame, bridge) = blurred_chain_radii(Affine::IDENTITY, [30, 30, 1, 1]);
        let (hits, grids) = check_rotation_grid(frame, bridge, &transforms, limit, false, None);
        assert!(hits > 0, "repeated poses must still reuse submitted pixels");
        assert_ne!(
            grids[0], grids[2],
            "rotation keeps its demanded axis densities"
        );
        assert!(grids.iter().all(|size| *size != [121, 91]));
    }
}

#[test]
#[ignore = "actual GPU warm fixed-grid input admission below the cold scratch budget"]
fn cached_prefix_admits_a_grid_that_exceeds_the_cold_scratch_budget() {
    let (frame, bridge) = blurred_chain_radii(Affine::IDENTITY, [30, 30, 1, 30]);
    // A 64-square source keeps the physical mapping unchanged when the budget
    // tightens. Only a retained input can remove the second scratch owner.
    let transforms = [Affine::scale_non_uniform(1., 4. / 3.); 4];
    let (hits, grids) = check_rotation_grid(
        frame,
        bridge,
        &transforms,
        64 << 20,
        false,
        Some((2, 40_000)),
    );
    assert!(hits > 0);
    assert!(grids.iter().all(|size| *size == [64, 64]));
}

#[test]
#[ignore = "requires REEL_FRAME_ARCHIVE and REEL_FRAME_DRAW_INDEX; actual recorded image, live rotating blur chain"]
fn archived_image_rotation_keeps_visible_work_bounded_and_cache_pixels_exact() {
    let pool = reel_core::budget::BudgetPool::new(
        reel_core::budget::BudgetCategory::CpuOutput,
        reel_core::budget::QueueLimits {
            max_requests: 1024,
            max_bytes: 256 << 20,
        },
    );
    let image = archive::load(
        &pool,
        reel_ui::frame::BuildLimits {
            max_bytes: 32 << 20,
        },
    )
    .unwrap();
    assert!(image.image.appearance.is_none());
    assert!(image.image.source_rect.is_none());
    assert_eq!(image.image.color, [1.; 4]);
    assert_eq!(image.composite, Composite::default());
    assert_eq!(image.brush_transform, Some(Affine::IDENTITY));
    assert_eq!(image.fill_rule, Default::default());
    let (bounds, effect_to_frame) = image.enclosing_effect.unwrap();
    let placement = effect_to_frame.inverse() * image.transform;
    let coverage = image.lease.image_coverage().unwrap();
    eprintln!(
        "archived_rotation image={:?} coverage_full={} coverage_rects={} original_transform={:?}",
        image.lease.image_size(),
        coverage.is_full(),
        coverage.rectangles().count(),
        image.transform
    );
    let mut widget = r01_fixture::RecordWidget::new(Scene::new());
    widget.clip = false;
    widget
        .resource_draws
        .push((image.lease.clone(), image.bounds, placement, image.sampler));
    let center = bounds.center();
    let info = FrameInfo {
        logical_size: image.logical_size,
        physical_size: image.physical_size,
    };
    let transforms = [
        0., 0., 0.11, 0.37, 0.37, 0.37, 0.82, -0.26, -0.26, -0.26, -0.82,
    ]
    .map(|angle| {
        Affine::scale_non_uniform(
            image.logical_size[0] / f64::from(image.physical_size[0]),
            image.logical_size[1] / f64::from(image.physical_size[1]),
        ) * Affine::translate((
            f64::from(image.physical_size[0]) * 0.5,
            f64::from(image.physical_size[1]) * 0.5,
        )) * Affine::scale_non_uniform(1.875, 0.609375)
            * Affine::rotate(angle)
            * Affine::translate((-center.x, -center.y))
    });
    let (original, bridge) = build_blur_chain(
        widget,
        vec![image.lease],
        bounds,
        info,
        Affine::IDENTITY,
        [30, 30, 1, 1],
    );
    let (hits, grids) = check_rotation_grid(original, bridge, &transforms, 90 << 20, false, None);
    assert!(
        hits > 0,
        "actual source must reuse submitted intermediate pixels"
    );
    assert!(grids.iter().all(|size| *size != [2866, 2866]));
    assert_ne!(grids[0], grids[2]);
}

fn check_rotation_grid(
    original: RenderFrame,
    mut bridge: PresentationBridge,
    transforms: &[Affine],
    target_limit: u64,
    experiment: bool,
    budget_after: Option<(usize, u64)>,
) -> (usize, Vec<[u32; 2]>) {
    use reel_ui::frame::FrameOp;
    let owner = original
        .commands()
        .iter()
        .find_map(|command| match &command.op {
            FrameOp::PushGroup(group) if group.effect.is_some() => Some(group.owner),
            _ => None,
        })
        .unwrap();
    let pool = r01_fixture::pool();
    let mut exact = renderer(target_limit);
    let mut fresh = renderer(target_limit);
    let mut cached = renderer(target_limit);
    let mut moved_hits = 0;
    let mut grid = None;
    let mut grids = Vec::new();
    for (index, &transform) in transforms.iter().enumerate() {
        bridge.edit_widget(owner, |mut widget| widget.set_transform(transform));
        let mut build = FrameBuild::new(
            bridge.record(FrameId(index as u64 + 2)).unwrap(),
            ResourceSnapshot::capture(original.resources().leases().iter().cloned().map(Ok), &pool)
                .unwrap(),
            original.info(),
            r01_fixture::limits(),
            &pool,
        )
        .unwrap();
        while !build.step(r01_fixture::step_budget()).unwrap().complete {}
        let frame = build.finish().unwrap();
        exact.effect_results.clear();
        let reference = with_enabled(false, || prepare_color8(&exact, &frame));
        let reference = exact.readback(reference).unwrap();
        while fresh.effect_results.reclaim_output() {}
        let (reference_grid, source_passes) =
            rotation_policy(experiment, || collect(|| prepare_color8(&fresh, &frame)));
        if let Some((first, limit)) = budget_after
            && index >= first
        {
            assert!(
                reference_grid.budget_scope.targets.usage().peak_bytes > limit,
                "the same-grid cold chain must exceed the tightened device budget"
            );
            cached.gpu.limits.target_bytes = limit;
            if index == first {
                cached.targets.release_idle();
            }
        }
        let source_pixels: u64 = source_passes
            .iter()
            .take(source_passes.len() - 1)
            .map(|(_, _, scissor)| u64::from(scissor.width) * u64::from(scissor.height))
            .sum();
        if !experiment && original.info().physical_size == [960, 544] {
            assert!(
                source_pixels < 3 * 2866 * 2866 / 2,
                "a visible window must not warm three complete 2866-square passes"
            );
        }
        let expected = fresh.readback(reference_grid).unwrap();
        let current_grid = source_passes[0].1;
        grids.push(current_grid);
        if experiment {
            assert_eq!(*grid.get_or_insert(current_grid), current_grid);
        }
        let ((candidate, passes), inputs) = rotation_policy(experiment, || {
            crate::common::effect_inputs::testing::collect(|| {
                collect(|| prepare_color8(&cached, &frame))
            })
        });
        let physical_bytes = candidate.budget_scope.targets.usage().bytes;
        let peak_bytes = candidate.budget_scope.targets.usage().peak_bytes;
        assert!(peak_bytes <= cached.gpu.limits.target_bytes);
        if let Some((first, _)) = budget_after
            && index >= first
        {
            assert!(
                inputs.iter().any(|sample| sample.inputs > 0),
                "cached inputs must admit the otherwise unaffordable raster"
            );
            assert!(
                candidate
                    .effect_pins
                    .iter()
                    .any(|target| target.size == current_grid),
                "the selected cached physical owner must retain the fixed grid"
            );
        }
        assert_eq!(candidate.quality().format, wgpu::TextureFormat::Rgba8Unorm);
        assert_eq!(candidate.quality().samples, 1);
        let actual = cached.readback(candidate).unwrap();
        let cache_changed = actual.iter().zip(&expected).filter(|(a, b)| a != b).count();
        assert_eq!(
            cache_changed, 0,
            "cache changed selected-grid pixels at pose={index}"
        );
        let hits: usize = inputs.iter().map(|input| input.inputs).sum();
        if index > 1 {
            moved_hits += hits;
        }
        let changed = reference
            .iter()
            .zip(&actual)
            .filter(|(a, b)| a != b)
            .count();
        let max_error = reference
            .iter()
            .zip(&actual)
            .flat_map(|(a, b)| a.iter().zip(b))
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        eprintln!(
            "orientation_raster pose={index} grid={current_grid:?} inputs={hits} effects={} fresh_effects={} source_pixels={source_pixels} physical_bytes={physical_bytes} peak_bytes={peak_bytes} cached_changed_pixels={cache_changed} original_changed_pixels={changed} max_linear_error={max_error}",
            passes.len(),
            source_passes.len()
        );
    }
    eprintln!("orientation_raster moving_input_hits={moved_hits}");
    (moved_hits, grids)
}

fn rotation_policy<T>(experiment: bool, run: impl FnOnce() -> T) -> T {
    if experiment {
        crate::common::effect_plan::raster_scale::testing::with_orientation_stable(run)
    } else {
        run()
    }
}

fn prepare_color8(renderer: &Renderer, frame: &RenderFrame) -> crate::gpu::Prepared {
    // Use the existing ceiling at the Vita color quality for every pose, rather
    // than allowing a smaller reference raster to select a different format.
    *renderer.retained.epoch.lock().unwrap() = Some(frame.surface_epoch());
    *renderer.retained.ceiling.lock().unwrap() = Some((
        frame.key(),
        crate::gpu::Quality {
            format: wgpu::TextureFormat::Rgba8Unorm,
            samples: 1,
            degraded: true,
        },
    ));
    renderer.prepare(frame).unwrap()
}

#[test]
#[ignore = "actual GPU experimental density, texture/rotation and cache pixel comparison"]
fn upward_source_raster_keeps_same_grid_cached_pixels_exact() {
    let mut control = renderer(50 << 10);
    let mut subject = renderer(50 << 10);
    let mut changed_total = 0;
    let mut input_hits = 0;
    for (case, (angle, sx, sy, tx, ty)) in [
        (0., 0.837, 1.213, 0.125, -0.375),
        (0.37, 0.837, 1.213, -3.7, 5.3),
        (-0.82, 1.117, 0.763, 9.7, -7.3),
        (0.31, -0.913, 1.071, -0.5, 0.5),
    ]
    .into_iter()
    .enumerate()
    {
        let transform = Affine::translate((32. + tx, 24. + ty))
            * Affine::rotate(angle)
            * Affine::scale_non_uniform(sx, sy)
            * Affine::translate((-32., -24.));
        // Existing styled texture includes transparent texels, per-pass tint,
        // seven RGBA stores, noninteger scale, rotation and reflected geometry.
        let (frame, _recording) = if case % 2 == 0 {
            transformed_chain([64, 48], transform, true)
        } else {
            blurred_chain(transform)
        };
        control.effect_results.clear();
        subject.effect_results.clear();
        let (baseline, exact_passes) =
            with_enabled(false, || collect(|| control.prepare(&frame).unwrap()));
        let quality = baseline.quality();
        assert_eq!(quality.format, wgpu::TextureFormat::Rgba8Unorm);
        assert_eq!(quality.samples, 1);
        let exact = control.readback(baseline).unwrap();
        let (prepared, selected_passes) =
            with_enabled(true, || collect(|| subject.prepare(&frame).unwrap()));
        assert_eq!(
            prepared.quality(),
            quality,
            "density experiment must not change format/MSAA"
        );
        let usage = subject.usage();
        let fresh = subject.readback(prepared).unwrap();
        assert!(
            fresh.iter().any(|pixel| pixel[3] > 0.1),
            "visible translucent source at case {case}"
        );
        assert_eq!(selected_passes.len(), exact_passes.len());
        assert!(selected_passes.len() >= 3);
        let exact_grid = exact_passes[0].1;
        let selected_grid = selected_passes[0].1;
        assert_ne!(selected_grid, exact_grid);
        assert!(selected_grid.iter().zip(exact_grid).all(|(&a, b)| a >= b));
        assert!(
            selected_passes[..selected_passes.len() - 1]
                .iter()
                .all(|pass| pass.1 == selected_grid),
            "nested effects must inherit one grid"
        );
        let (cached, inputs) = with_enabled(true, || {
            crate::common::effect_inputs::testing::collect(|| subject.prepare(&frame).unwrap())
        });
        assert_eq!(cached.quality(), quality);
        input_hits += inputs.iter().map(|input| input.inputs).sum::<usize>();
        assert_eq!(
            subject.readback(cached).unwrap(),
            fresh,
            "same selected grid cache versus fresh at case {case}"
        );
        control.effect_results.clear();
        let reference = with_enabled(true, || control.prepare(&frame).unwrap());
        assert_eq!(reference.quality(), quality);
        assert_eq!(
            control.readback(reference).unwrap(),
            fresh,
            "independent fresh grid reference"
        );
        let changed = exact.iter().zip(&fresh).filter(|(a, b)| a != b).count();
        let max = exact
            .iter()
            .zip(&fresh)
            .flat_map(|(a, b)| a.iter().zip(b))
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        changed_total += changed;
        eprintln!(
            "source_raster_pixels case={case} exact_grid={exact_grid:?} selected_grid={selected_grid:?} quality={quality:?} usage={usage:?} exact_vs_selected_changed_pixels={changed} max_linear_error={max} cached_vs_fresh_changed_pixels=0"
        );
    }
    assert!(
        input_hits > 0,
        "the pixel comparison must exercise prepared cached inputs"
    );
    assert!(
        changed_total > 0,
        "higher density is an explicitly different pixel policy"
    );
}
