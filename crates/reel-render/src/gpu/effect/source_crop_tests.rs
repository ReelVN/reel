//! Exact pixel and lifetime comparisons through production frame planning/replay.
use super::*;
#[path = "output_crop_tests.rs"]
#[cfg(feature = "profiling")]
mod output_crop_tests;

const SOURCE: [u32; 2] = [2048, 512];
const OUTPUT: [u32; 2] = [640, 256];

fn frame(proved: bool, depth: usize, offset: [f64; 2]) -> (RenderFrame, PresentationBridge) {
    frame_with_extra(proved, depth, offset, false)
}

fn frame_with_extra(
    proved: bool,
    depth: usize,
    offset: [f64; 2],
    extra: bool,
) -> (RenderFrame, PresentationBridge) {
    frame_config(
        proved,
        depth,
        Affine::translate((offset[0], offset[1])),
        extra,
        false,
    )
}

fn frame_config(
    proved: bool,
    depth: usize,
    transform: Affine,
    extra: bool,
    complex_source: bool,
) -> (RenderFrame, PresentationBridge) {
    let pool = r01_fixture::pool();
    let mut registry = ResourceRegistry::new(8, 4 << 20).unwrap();
    let version = |id| ResourceVersion {
        key: AssetKey(format!("crop-{id}")),
        version: ContentVersion([id; 32]),
    };
    let pixels: Vec<_> = (0..128 * 32)
        .flat_map(|i| {
            let alpha = (64 + i * 71 % 192) as u8;
            [
                (i * 17 % usize::from(alpha)) as u8,
                (i * 31 % usize::from(alpha)) as u8,
                (i * 47 % usize::from(alpha)) as u8,
                alpha,
            ]
        })
        .collect();
    let image = registry
        .register_image(
            version(20),
            OwnedBytes::new(pixels, pool.reserve(128 * 32 * 4).unwrap()).unwrap(),
            [128, 32],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Linear,
        )
        .unwrap();
    let mut scene = Scene::new();
    let sampled = crate::test_support::image_data(&registry.acquire(image).unwrap());
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Image(ImageBrush::new(sampled)),
        brush_transform: Some(Affine::scale(1. / 16.)),
        shape: Geometry::Rect(Rect::new(0., 0., 2048., 512.)),
        composite: Composite {
            alpha: 0.625,
            ..Default::default()
        },
    });
    if complex_source {
        scene.draw(Draw::Fill {
            transform: Affine::rotate(0.03),
            fill_rule: Default::default(),
            brush: Brush::Solid(reel_ui::peniko::Color::from_rgba8(23, 71, 113, 137)),
            brush_transform: None,
            shape: Geometry::Rect(Rect::new(197., 61., 973., 193.)),
            composite: Default::default(),
        });
    }
    let mut widget = r01_fixture::RecordWidget::new(scene);
    widget.clip = false;
    widget.inner.clip = Rect::new(0., 0., 2048., 512.);
    let mut handles = vec![image];
    for index in 0..depth {
        let program = kernel((index % 2) as u8, 1);
        let fragment = compile_fragment(&program, Language::Wgsl, 1 << 20).unwrap();
        let id = index as u8 + 1;
        let descriptor = EffectShaderDescriptor::with_textures(
            [id; 32],
            EffectShaderFormat::Wgsl,
            vec![],
            0,
            true,
            EffectType::Vec4,
            fragment.source.as_bytes(),
        )
        .unwrap()
        .with_source_footprint(
            proved.then(|| source_footprint(&program).unwrap()),
            fragment.source.as_bytes(),
        )
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
        parent.inner.clip = Rect::new(0., 0., 2048., 512.);
        parent.effect = Some(PaintEffect {
            instance: EffectInstance::new([id; 32], program, vec![]).unwrap(),
            shader: registry.acquire(shader).unwrap(),
            images: vec![],
            bounds: Rect::new(0., 0., 2048., 512.),
            coverage_invert: false,
            resolution_scale: 1.,
        });
        parent
            .inner
            .add(widget.prepare(), Point::ZERO, Size::new(2048., 512.));
        widget = parent;
    }
    let mut stage = r01_fixture::RecordWidget::new(Scene::new());
    stage.clip = true;
    stage.inner.clip = Rect::new(0., 0., 640., 256.);
    stage.inner.add(
        widget.prepare().with_transform(transform),
        Point::ZERO,
        Size::new(2048., 512.),
    );
    if extra {
        let mut scene = Scene::new();
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Solid(reel_ui::peniko::Color::from_rgba8(32, 48, 64, 128)),
            brush_transform: None,
            shape: Geometry::Rect(Rect::new(0., 0., 333., 87.)),
            composite: Default::default(),
        });
        let child = r01_fixture::RecordWidget::new(scene);
        let mut effect = r01_fixture::RecordWidget::new(Scene::new());
        effect.clip = false;
        effect.inner.clip = Rect::new(0., 0., 333., 87.);
        effect.effect = Some(PaintEffect {
            instance: EffectInstance::new([1; 32], kernel(0, 1), vec![]).unwrap(),
            shader: registry.acquire(handles[1]).unwrap(),
            images: vec![],
            bounds: Rect::new(0., 0., 333., 87.),
            coverage_invert: false,
            resolution_scale: 1.,
        });
        effect
            .inner
            .add(child.prepare(), Point::ZERO, Size::new(333., 87.));
        stage
            .inner
            .add(effect.prepare(), Point::ZERO, Size::new(333., 87.));
    }
    let root = RenderRoot::new(
        stage.prepare(),
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(640, 256),
            scale_factor: 1.,
            default_font: None,
        },
    );
    let mut bridge = r01_fixture::bridge(root);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&handles, &pool).unwrap(),
        FrameInfo {
            logical_size: [640., 256.],
            physical_size: OUTPUT,
        },
        r01_fixture::limits(),
        &pool,
    )
    .unwrap();
    while !build.step(r01_fixture::step_budget()).unwrap().complete {}
    (build.finish().unwrap(), bridge)
}

#[test]
fn unrelated_dynamic_storage_keeps_selected_crop_exact_and_replay_requirements_identical() {
    use crate::common::{
        glyph_plan, offscreen,
        target_plan::{Input, MaskExecution},
    };
    let area = |size: [u32; 2]| u64::from(size[0]) * u64::from(size[1]) * 4;
    for depth in [1, 2, 4] {
        let mut ordinary = 0;
        for proved in [false, true] {
            let (frame, _recording) = frame_with_extra(proved, depth, [-1200., -128.], true);
            let selection = Default::default();
            let glyphs = glyph_plan::Plan::default();
            let cost = |size| Ok(area(size));
            let (schedule, bytes, _) = offscreen::Plan::choose(
                &frame,
                None,
                OUTPUT,
                false,
                1,
                4096,
                &glyphs,
                u64::MAX,
                &cost,
                MaskExecution::Inline,
                &selection,
                |_| Ok(()),
            )
            .unwrap();
            assert_ne!(
                schedule.storage_size([333, 87]),
                [333, 87],
                "unrelated dynamic class is padded"
            );
            if proved {
                assert!(schedule.source_crops());
                assert_eq!(schedule.storage_size(SOURCE), SOURCE);
                assert_eq!(schedule.storage_size([1024, 512]), [1024, 512]);
                assert!(schedule.requirements().count([1024, 512]) > 0);
            } else {
                ordinary = bytes;
            }
            let replay = Input {
                frame: &frame,
                second: None,
                size: OUTPUT,
                capture: false,
                samples: 1,
                max_dimension: 4096,
                glyphs: &glyphs,
                schedule: &schedule,
                masks: MaskExecution::Inline,
                selection: &selection,
                cost: &cost,
                coverage_cost: None,
            }
            .measure(|_| Ok(()))
            .unwrap();
            let physical = replay
                .requirements(|_| Ok(()))
                .unwrap()
                .iter()
                .map(|(logical, count)| area(schedule.storage_size(logical)) * count)
                .sum::<u64>();
            assert_eq!(replay.cropped, proved);
            assert_eq!(
                physical, bytes,
                "physical requirements must equal the actual selected replay"
            );
            if proved && depth == 4 {
                let (fallback, bytes, _) = offscreen::Plan::choose(
                    &frame,
                    None,
                    OUTPUT,
                    false,
                    1,
                    4096,
                    &glyphs,
                    ordinary,
                    &cost,
                    MaskExecution::Inline,
                    &selection,
                    |_| Ok(()),
                )
                .unwrap();
                assert!(!fallback.source_crops());
                assert_eq!(fallback.requirements().count([1024, 512]), 0);
                assert!(bytes <= ordinary);
            }
        }
    }
}

#[test]
#[ignore = "actual adapter; strict RGBA8 readback and source allocation counts"]
fn source_crop_preserves_raw_chain_pixels_and_reduces_real_backing() {
    for depth in [1, 2, 4] {
        for offset in [[-1200., -128.], [-1200.5, -128.25], [-1408., 0.]] {
            let mut cases = Vec::new();
            for proved in [false, true] {
                let (frame, _recording) = frame(proved, depth, offset);
                let limit = 4
                    * (u64::from(OUTPUT[0]) * u64::from(OUTPUT[1])
                        + u64::from(SOURCE[0])
                            * u64::from(SOURCE[1])
                            * if depth == 1 { 1 } else { 2 })
                    + if depth == 4 {
                        2 * u64::from(SOURCE[0]) * u64::from(SOURCE[1])
                    } else {
                        0
                    };
                let gpu = pollster::block_on(Gpu::request(
                    &wgpu::Instance::default(),
                    None,
                    DeviceGeneration(1),
                    GpuLimits {
                        target_bytes: limit,
                        ..Default::default()
                    },
                ))
                .unwrap();
                let mut renderer = Renderer::new(gpu);
                let (prepared, targets) =
                    crate::gpu::targets::pass_trace::collect(|| renderer.prepare(&frame).unwrap());
                assert_eq!(prepared.quality().format, wgpu::TextureFormat::Rgba8Unorm);
                assert_eq!(prepared.quality().samples, 1);
                let areas: Vec<_> = targets
                    .iter()
                    .map(|texture| {
                        let s = texture.size();
                        u64::from(s.width) * u64::from(s.height)
                    })
                    .collect();
                let pixels = renderer.readback(prepared).unwrap();
                cases.push((pixels, areas));
            }
            assert_eq!(
                cases[0].0, cases[1].0,
                "all RGBA bytes: depth={depth} offset={offset:?}"
            );
            assert!(
                cases[1].1.iter().sum::<u64>() < cases[0].1.iter().sum::<u64>(),
                "actual target payloads must shrink: depth={depth} offset={offset:?} {:?} -> {:?}",
                cases[0].1,
                cases[1].1
            );
            eprintln!(
                "source crop depth={depth} offset={offset:?} target_pixels={:?}->{:?} exact_pixels={}",
                cases[0].1,
                cases[1].1,
                cases[1].0.len()
            );
        }
    }
}

#[test]
fn real_raw_chain_walk_accounts_crop_classes_and_remeasures_tight_fallback() {
    use crate::common::{glyph_plan, offscreen, target_plan::MaskExecution};
    let area = |size: [u32; 2]| u64::from(size[0]) * u64::from(size[1]);
    let source = area(SOURCE) * 4;
    let root = area(OUTPUT) * 4;
    for depth in [1, 2, 4] {
        let mut measured = Vec::new();
        for proved in [false, true] {
            let (frame, _recording) = frame(proved, depth, [-1200., -128.]);
            let started = std::time::Instant::now();
            let (schedule, bytes, _) = offscreen::Plan::choose(
                &frame,
                None,
                OUTPUT,
                false,
                1,
                4096,
                &glyph_plan::Plan::default(),
                u64::MAX,
                &|size| Ok(area(size) * 4),
                MaskExecution::Inline,
                &Default::default(),
                |_| Ok(()),
            )
            .unwrap();
            eprintln!(
                "crop raw chain depth={depth} proved={proved} requirements_bytes={bytes} plan_wall_us={}",
                started.elapsed().as_micros()
            );
            if proved {
                assert!(schedule.requirements().count([1024, 512]) != 0);
            }
            measured.push(bytes);
        }
        assert_eq!(measured[0], root + source * if depth == 1 { 1 } else { 2 });
        assert_eq!(
            measured[1],
            root + source / 2
                + source
                    * if depth == 1 {
                        0
                    } else if depth == 2 {
                        1
                    } else {
                        2
                    }
        );
        if depth == 4 {
            let (frame, _recording) = frame(true, depth, [-1200., -128.]);
            let (schedule, bytes, _) = offscreen::Plan::choose(
                &frame,
                None,
                OUTPUT,
                false,
                1,
                4096,
                &glyph_plan::Plan::default(),
                measured[0],
                &|size| Ok(area(size) * 4),
                MaskExecution::Inline,
                &Default::default(),
                |_| Ok(()),
            )
            .unwrap();
            assert!(!schedule.source_crops());
            assert_eq!(bytes, measured[0]);
            assert_eq!(schedule.requirements().count([1024, 512]), 0);
            assert_eq!(schedule.requirements().count(SOURCE), 2);
        }
    }
}
