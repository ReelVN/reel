//! Glyph rasterization, atlas reuse and text batches.
use super::*;

#[test]
#[ignore = "requires REEL_R01_TEST_FONT; CPU batch parity and cost"]
fn parallel_glyph_batches_preserve_pixels_errors_and_budget_ownership() {
    use crate::common::glyph::{RasterLimits, RasterRun, Rasterizer};
    let (_pool, _registry, _handle, frame, _bridge) = long_text_fixture();
    let budgets = Budgets::new(Default::default());
    let limits = RasterLimits {
        available: u64::MAX,
        max_dimension: 8192,
        budget: budgets.transient.clone(),
    };
    let mut serial = Rasterizer::default();
    let batch_context = crate::common::glyph::Rasterizers::default();
    let mut serial_time = std::time::Duration::ZERO;
    let mut parallel_time = std::time::Duration::ZERO;
    let mut glyphs = 0;
    for command in frame.commands() {
        let reel_ui::frame::FrameOp::Draw(reel_ui::frame::FrameDraw::GlyphRun(run)) = &command.op
        else {
            continue;
        };
        let mut indices = Vec::new();
        for (index, position) in run.glyphs.iter().enumerate() {
            if !indices
                .iter()
                .any(|&prior: &usize| run.glyphs[prior].id == position.id)
            {
                indices.push(index);
            }
            if indices.len() == 32 {
                break;
            }
        }
        let raster = RasterRun::new(frame.drawing(), run, 1., command.origin.at).unwrap();
        let start = Instant::now();
        let expected: Vec<_> = indices
            .iter()
            .map(|&index| raster.glyph(&mut serial, index, limits.clone()).unwrap())
            .collect();
        serial_time += start.elapsed();
        let start = Instant::now();
        let actual = raster.batch(&batch_context, &indices, limits.clone());
        parallel_time += start.elapsed();
        glyphs += indices.len();
        for (actual, expected) in actual.into_iter().zip(expected) {
            let pixels = |bitmap: Option<crate::common::glyph::GlyphBitmap>| {
                bitmap.map(|bitmap| (bitmap.x, bitmap.y, bitmap.size, bitmap.alpha))
            };
            assert_eq!(pixels(actual.unwrap()), pixels(expected));
        }
        assert_eq!(budgets.transient.usage().bytes, 0);
        let mut invalid = run.clone();
        invalid.glyphs[indices[0]].id = u32::MAX;
        let raster = RasterRun::new(frame.drawing(), &invalid, 1., command.origin.at).unwrap();
        let results = raster.batch(&batch_context, &indices, limits.clone());
        assert!(
            matches!(&results[0], Err(GpuError::Unsupported { at, record: "glyph id" }) if *at == command.origin.at)
        );
        drop(results);
        assert_eq!(budgets.transient.usage().bytes, 0);
    }
    assert!(glyphs > 32);
    eprintln!(
        "glyph_batch glyphs={glyphs} serial_us={} parallel_us={}",
        serial_time.as_micros(),
        parallel_time.as_micros()
    );
}

#[test]
#[ignore = "requires actual graphics adapter and REEL_R01_TEST_FONT"]
fn retained_text_preparation_publishes_glyphs_for_the_following_draw() {
    use reel_ui::frame::{FrameTransition, RetainBuild};
    let (pool, registry, handle, frame, mut bridge) = long_text_fixture();
    let mut retain = RetainBuild::new(&frame, None, limits(), &pool).unwrap();
    let mut budget = step_budget();
    loop {
        let progress = retain.step(budget).unwrap();
        if progress.complete {
            break;
        }
        budget.max_units = budget.max_units.max(progress.minimum_units);
        budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
    }
    let mut build = FrameBuild::new(
        bridge.record(FrameId(2)).unwrap(),
        registry.freeze(&[handle], &pool).unwrap(),
        frame.info(),
        limits(),
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
    while !build.step(step_budget()).unwrap().complete {}
    let transitioning = build.finish().unwrap();
    let mut subject = renderer(Default::default());
    assert!(matches!(
        subject.prepare(&transitioning),
        Err(GpuError::Preparing)
    ));
    let glyphs: Vec<_> = subject
        .retained
        .pending
        .lock()
        .unwrap()
        .entries
        .iter()
        .flat_map(|pending| {
            pending
                .flight
                ._glyphs
                .entries
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
        })
        .collect();
    assert!(!glyphs.is_empty());
    assert!(
        subject
            .retained
            .pending
            .lock()
            .unwrap()
            .entries
            .iter()
            .all(|pending| {
                !pending
                    .flight
                    .done
                    .load(std::sync::atomic::Ordering::Acquire)
            })
    );
    let prepared = subject.prepare(&transitioning).unwrap();
    for (key, glyph) in &glyphs {
        assert!(
            Arc::ptr_eq(prepared.glyphs.entries.get(key).unwrap(), glyph),
            "the final drawing must reuse glyphs uploaded with the retained input"
        );
    }
    let actual = subject.readback(prepared).unwrap();
    let mut control = renderer(Default::default());
    let expected = control.readback(control.prepare(&frame).unwrap()).unwrap();
    assert_eq!(actual.len(), expected.len());
    let error = actual
        .iter()
        .zip(expected)
        .flat_map(|(a, b)| a.iter().copied().zip(b))
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        error < 0.002,
        "unchanged text crossfade changed pixels: {error}"
    );
}

#[test]
#[ignore = "requires REEL_R01_TEST_FONT; alternating font raster cost and pixel equivalence"]
fn alternating_font_raster_cost() {
    use reel_ui::{
        app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
        core::*,
        dpi::PhysicalSize,
        peniko::Blob,
        resources::{ResourceData, ResourceRegistry},
    };
    let pool = pool();
    let bytes = std::fs::read(std::env::var("REEL_R01_TEST_FONT").unwrap()).unwrap();
    let mut registry = ResourceRegistry::new(2, 65264).unwrap();
    let mut handles = Vec::new();
    let mut fonts = Vec::new();
    for index in 0..2 {
        let storage = pool.reserve(bytes.len() as u64).unwrap();
        let font = Blob::from(bytes.clone());
        handles.push(
            registry
                .register(
                    reel_core::identity::ResourceVersion {
                        key: reel_core::identity::AssetKey(format!("font-{index}").into()),
                        version: reel_core::identity::ContentVersion([index; 32]),
                    },
                    ResourceData::Font(font.clone()),
                    storage,
                )
                .unwrap(),
        );
        fonts.push(font);
    }
    let root = RenderRoot::new(
        NewWidget::new(reel_ui::widgets::Label::new("M").with_style(StyleProperty::FontSize(24.))),
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(100, 100),
            scale_factor: 1.,
            default_font: Some(fonts[0].clone()),
        },
    );
    let mut bridge = r01_fixture::bridge(root);
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&handles, &pool).unwrap(),
        &pool,
    );
    let (run, at) = frame
        .commands()
        .iter()
        .find_map(|command| match &command.op {
            reel_ui::frame::FrameOp::Draw(reel_ui::frame::FrameDraw::GlyphRun(run)) => {
                Some((run, command.origin.at))
            }
            _ => None,
        })
        .unwrap();
    let mut run = run.clone();
    run.hint = true;
    let mut raster = crate::common::glyph::Rasterizer::default();
    let mut reference = None;
    let began = std::time::Instant::now();
    for iteration in 0..2000 {
        run.font = handles[iteration % 2];
        let bitmap = crate::common::glyph::rasterize(
            &mut raster,
            frame.drawing(),
            &run,
            1.,
            0,
            at,
            crate::common::glyph::RasterLimits {
                available: u64::MAX,
                max_dimension: 8192,
                budget: pool.clone(),
            },
        )
        .unwrap()
        .unwrap();
        let value = (bitmap.x, bitmap.y, bitmap.size, bitmap.alpha);
        if let Some(reference) = &reference {
            assert_eq!(&value, reference);
        } else {
            reference = Some(value);
        }
    }
    println!(
        "alternating_font_raster glyphs=2000 elapsed_us={}",
        began.elapsed().as_micros()
    );
}

#[test]
#[ignore = "requires REEL_R01_TEST_FONT; CPU only, no graphics adapter"]
fn transformed_text_raster_keeps_logical_metrics_and_scales_fill_and_outline() {
    use reel_ui::{
        app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
        core::*,
        dpi::PhysicalSize,
        peniko::Blob,
        resources::{ResourceData, ResourceRegistry},
    };
    let pool = pool();
    let bytes = std::fs::read(std::env::var("REEL_R01_TEST_FONT").unwrap()).unwrap();
    let reservation = pool.reserve(bytes.len() as u64).unwrap();
    let font = Blob::from(bytes);
    let mut registry = ResourceRegistry::new(1, 65264).unwrap();
    let handle = registry
        .register(
            reel_core::identity::ResourceVersion {
                key: reel_core::identity::AssetKey("scaled-font".into()),
                version: reel_core::identity::ContentVersion([1; 32]),
            },
            ResourceData::Font(font.clone()),
            reservation,
        )
        .unwrap();
    let root = RenderRoot::new(
        NewWidget::new(
            reel_ui::widgets::Label::new("M")
                .with_style(StyleProperty::FontSize(24.))
                .with_outline(Some(TextOutline {
                    width: 2.,
                    color: Color::WHITE,
                })),
        ),
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
    let mut bridge = r01_fixture::bridge(root);
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[handle], &pool).unwrap(),
        &pool,
    );
    let mut raster = crate::gpu::glyph::Rasterizer::default();
    let density = crate::gpu::glyph::raster_density;
    assert_eq!(density(Affine::translate((123., 42.))), 1.);
    assert_eq!(density(Affine::scale(1.25)), std::f64::consts::SQRT_2);
    assert_eq!(density(Affine::scale(1.75)), 2.);
    assert_eq!(density(Affine::scale_non_uniform(3., 1.)), 4.);
    let mut fills = 0;
    let mut strokes = 0;
    for command in frame.commands() {
        let reel_ui::frame::FrameOp::Draw(reel_ui::frame::FrameDraw::GlyphRun(run)) = &command.op
        else {
            continue;
        };
        let normal = crate::gpu::glyph::rasterize(
            &mut raster,
            frame.drawing(),
            run,
            1.,
            0,
            command.origin.at,
            crate::gpu::glyph::RasterLimits {
                available: u64::MAX,
                max_dimension: 8192,
                budget: pool.clone(),
            },
        )
        .unwrap()
        .unwrap();
        for density in [0.25, 0.5, std::f64::consts::SQRT_2, 2.] {
            let scaled = crate::gpu::glyph::rasterize(
                &mut raster,
                frame.drawing(),
                run,
                density,
                0,
                command.origin.at,
                crate::gpu::glyph::RasterLimits {
                    available: u64::MAX,
                    max_dimension: 8192,
                    budget: pool.clone(),
                },
            )
            .unwrap()
            .unwrap();
            for axis in 0..2 {
                assert!(
                    (f64::from(scaled.size[axis]) / density - f64::from(normal.size[axis])).abs()
                        <= (2. / density).max(2.)
                );
            }
            assert!((scaled.x / density - normal.x).abs() <= (1. / density).max(1.));
            assert!((scaled.y / density - normal.y).abs() <= (1. / density).max(1.));
            assert!(
                crate::gpu::glyph_cache::RunKey::new(run, 1.)
                    != crate::gpu::glyph_cache::RunKey::new(run, density)
            );
        }
        match run.style {
            reel_ui::peniko::Style::Fill(_) => fills += 1,
            _ => strokes += 1,
        }
    }
    assert!(fills > 0 && strokes > 0);
}

#[test]
#[ignore = "requires actual graphics adapter and REEL_R01_TEST_FONT"]
fn long_text_reuses_atlas_and_batches_glyphs_across_frames_and_aborted_preparations() {
    use std::sync::atomic::Ordering;
    let (pool, mut registry, handle, frame, _bridge) = long_text_fixture();
    drop(_bridge);
    let glyph_count: usize = frame
        .commands()
        .iter()
        .filter_map(|c| match &c.op {
            reel_ui::frame::FrameOp::Draw(reel_ui::frame::FrameDraw::GlyphRun(run)) => {
                Some(run.glyphs.len())
            }
            _ => None,
        })
        .sum();
    assert!(glyph_count > 4000);
    let runs = frame
        .commands()
        .iter()
        .filter_map(|c| match &c.op {
            reel_ui::frame::FrameOp::Draw(reel_ui::frame::FrameDraw::GlyphRun(run)) => Some(run),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        runs.iter().any(|run| run.glyph_transform.is_some()),
        "regular font synthesizes italic"
    );
    assert!(
        runs.iter()
            .any(|run| matches!(run.style, reel_ui::peniko::Style::Stroke(_)))
    );
    let mut renderer = renderer(crate::gpu::GpuLimits::default());
    eprintln!(
        "adapter={:?}, glyphs={glyph_count}",
        renderer.gpu.adapter.get_info()
    );
    let discarded = renderer.prepare(&frame).unwrap();
    let accepted = renderer.prepare(&frame).unwrap();
    drop(discarded);
    let pixels = renderer.readback(accepted).unwrap();
    assert!(pixels.iter().filter(|p| p[3] > 0.1).count() > 100);
    assert!(
        pixels
            .iter()
            .any(|p| p[0] > 0.3 && p[1] < 0.01 && p[2] < 0.01),
        "red outline remains outside fill"
    );
    assert!(
        pixels
            .iter()
            .any(|p| p[0] > 0.3 && p[1] > 0.3 && p[2] > 0.3),
        "white fill uses a separate coverage cache entry"
    );
    let run = runs
        .iter()
        .find(|r| matches!(r.style, reel_ui::peniko::Style::Stroke(_)))
        .unwrap();
    let at = frame.commands()[0].origin.at;
    let mut raster = crate::gpu::glyph::Rasterizer::default();
    assert!(matches!(
        crate::gpu::glyph::rasterize(
            &mut raster,
            frame.drawing(),
            run,
            1.,
            0,
            at,
            crate::gpu::glyph::RasterLimits {
                available: 1,
                max_dimension: 8192,
                budget: pool.clone(),
            }
        ),
        Err(GpuError::Budget {
            domain: "glyph stroke path",
            ..
        })
    ));
    assert!(matches!(
        crate::gpu::glyph::rasterize(
            &mut raster,
            frame.drawing(),
            run,
            1.,
            0,
            at,
            crate::gpu::glyph::RasterLimits {
                available: u64::MAX,
                max_dimension: 1,
                budget: pool.clone(),
            }
        ),
        Err(GpuError::Unsupported {
            record: "glyph raster exceeds device texture dimensions",
            ..
        })
    ));
    let warm = renderer.gpu.upload_creations.snapshot();
    let resident = renderer.usage().resources;
    let target_resident = renderer.usage().targets;
    assert!(
        resident <= 2 * 512 * 512,
        "single-channel atlas residency {resident}"
    );
    let started = Instant::now();
    let draws = renderer.gpu.upload_creations.draws.load(Ordering::Relaxed);
    for _ in 0..120 {
        renderer.submit(renderer.prepare(&frame).unwrap()).unwrap();
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
        assert_eq!(renderer.usage().resources, resident);
        assert_eq!(renderer.usage().targets, target_resident);
    }
    let uploads = renderer.gpu.upload_creations.snapshot();
    // An optional white coverage texture is independent of glyph count.
    assert!(uploads[0] - warm[0] <= 120);
    assert!(uploads[2] - warm[2] <= 120);
    let draws = renderer.gpu.upload_creations.draws.load(Ordering::Relaxed) - draws;
    assert!(
        draws / 120 < glyph_count / 10,
        "draws must be per run/page, not per glyph"
    );
    eprintln!(
        "120 frames {:?}: atlas={resident} bytes; new textures={}, uploads={}, draws/frame={}",
        started.elapsed(),
        uploads[0] - warm[0],
        uploads[2] - warm[2],
        draws / 120
    );
    registry.retire(handle);
    renderer.retire(&[]);
    assert_eq!(
        renderer.usage().resources,
        resident,
        "the old frame still owns the font"
    );
    drop(runs);
    drop(frame);
    renderer.retire(&[]);
    assert_eq!(
        renderer.usage().resources,
        0,
        "final completed frame releases cached glyph pages"
    );
}
#[test]
#[ignore = "requires native WGPU and REEL_R01_TEST_FONT; no pixel readback"]
fn long_text_single_channel_atlas_submits_and_reuses_coverage() {
    use std::sync::atomic::Ordering;
    let (_pool, _registry, _handle, frame, _bridge) = long_text_fixture();
    let glyph_count: usize = frame
        .commands()
        .iter()
        .filter_map(|command| match &command.op {
            reel_ui::frame::FrameOp::Draw(reel_ui::frame::FrameDraw::GlyphRun(run)) => {
                Some(run.glyphs.len())
            }
            _ => None,
        })
        .sum();
    assert!(glyph_count > 4000);
    let mut renderer = renderer(Default::default());
    let discarded = renderer.prepare(&frame).unwrap();
    let accepted = renderer.prepare(&frame).unwrap();
    let pipelines = renderer
        .pipelines
        .get(&renderer.gpu, accepted.quality())
        .unwrap();
    let cold_resource_peak = renderer.budgets.resources.usage().peak_bytes;
    drop(discarded);
    renderer.submit(accepted).unwrap();
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    let variants = if pipelines.bounded.is_some() { 27 } else { 21 };
    assert_eq!(
        renderer.pipelines.creations.snapshot(),
        [3, variants, variants, variants, 8]
    );
    let resident = renderer.usage().resources;
    assert!(resident > 0 && resident <= 2 * 512 * 512);
    assert_eq!(resident % (512 * 512), 0);
    let uploads = renderer.gpu.upload_creations.snapshot();
    eprintln!("cold glyph upload counts={uploads:?} peak_resource_bytes={cold_resource_peak}");
    let coverage = pipelines.direct_draws[crate::common::mesh::DirectPaint::Coverage as usize]
        .load(Ordering::Relaxed);
    let draws = renderer.gpu.upload_creations.draws.load(Ordering::Relaxed);
    let geometry_upload = renderer.geometry.uploaded_bytes.load(Ordering::Relaxed);
    let start = Instant::now();
    let mut prepare_us = 0;
    for _ in 0..8 {
        let prepare = Instant::now();
        let prepared = renderer.prepare(&frame).unwrap();
        prepare_us += prepare.elapsed().as_micros();
        renderer.submit(prepared).unwrap();
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
        assert_eq!(renderer.usage().resources, resident);
    }
    assert_eq!(renderer.gpu.upload_creations.snapshot(), uploads);
    assert_eq!(
        renderer.geometry.uploaded_bytes.load(Ordering::Relaxed),
        geometry_upload,
        "stable source recordings skip all geometry uploads"
    );
    let coverage = pipelines.direct_draws[crate::common::mesh::DirectPaint::Coverage as usize]
        .load(Ordering::Relaxed)
        - coverage;
    assert!(coverage > 0 && coverage / 8 < glyph_count / 10);
    assert_eq!(
        renderer.gpu.upload_creations.draws.load(Ordering::Relaxed) - draws,
        coverage
    );
    assert!(
        pipelines
            .direct_draws
            .iter()
            .take(crate::common::mesh::DirectPaint::Coverage as usize)
            .all(|count| count.load(Ordering::Relaxed) == 0)
    );
    eprintln!(
        "glyphs={glyph_count} atlas_bytes={resident} cold_resource_peak={cold_resource_peak} warm_frames=8 uploads=0 geometry_upload_bytes=0 coverage_draws={coverage} prepare_us={prepare_us} submit_complete_us={}",
        start.elapsed().as_micros()
    );
    // The identical immutable frame remains renderable after its recording dies;
    // this is also the uncached reference workload without copying source code.
    drop(_bridge);
    renderer.geometry.shapes.lock().unwrap().clear();
    renderer.geometry.glyphs.lock().unwrap().clear();
    let before = renderer.geometry.uploaded_bytes.load(Ordering::Relaxed);
    let mut prepare_us = 0;
    for _ in 0..8 {
        let start = Instant::now();
        let prepared = renderer.prepare(&frame).unwrap();
        prepare_us += start.elapsed().as_micros();
        renderer.submit(prepared).unwrap();
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
    }
    let bytes = renderer.geometry.uploaded_bytes.load(Ordering::Relaxed) - before;
    assert!(bytes > 0);
    eprintln!(
        "same_frame_without_live_recording frames=8 geometry_upload_bytes={bytes} prepare_us={prepare_us}"
    );
}
