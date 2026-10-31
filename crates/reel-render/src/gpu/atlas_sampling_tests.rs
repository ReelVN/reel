//! Finite numeric sampling checks, independent of game or visual acceptance.
use super::*;
use reel_ui::{
    frame::{SpriteInstance, SpriteInstances},
    kurbo::Affine,
    peniko::ImageQuality,
};
use std::sync::atomic::Ordering;

fn pattern(
    lease: ResourceLease,
    scale: f64,
    quality: ImageQuality,
    crop: Option<Rect>,
    pool: &BudgetPool,
) -> RenderFrame {
    let widget = Image::from_resource(lease.clone())
        .with_pattern([true; 2], scale, None)
        .with_sampling(quality)
        .with_source_rect(crop);
    let root = RenderRoot::new(
        widget.prepare(),
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(64, 48),
            scale_factor: 1.,
            default_font: None,
        },
    );
    let mut bridge = PresentationBridge::new(
        root,
        SurfaceEpoch(1),
        BridgeLimits {
            geometry: GeometryLimits { max_bytes: 1 << 20 },
            max_candidates: 2,
            max_bridge_bytes: 65536,
            max_snapshot_bytes: 4 << 20,
        },
    )
    .unwrap();
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        ResourceSnapshot::capture([Ok(lease)].into_iter(), pool).unwrap(),
        FrameInfo {
            logical_size: [64., 48.],
            physical_size: [64, 48],
        },
        BuildLimits { max_bytes: 1 << 20 },
        pool,
    )
    .unwrap();
    while !build
        .step(StepBudget {
            max_commands: 128,
            max_units: 65536,
            max_bytes: 1 << 20,
        })
        .unwrap()
        .complete
    {}
    build.finish().unwrap()
}
fn error(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    a.iter()
        .zip(b)
        .flat_map(|(a, b)| a.iter().zip(b).map(|(a, b)| (a - b).abs()))
        .fold(0., f32::max)
}
fn oracle(bytes: &[u8], scale: f64, quality: ImageQuality, crop: Option<Rect>) -> Vec<[f32; 4]> {
    let texel = |x: i32, y: i32| {
        std::array::from_fn(|c| {
            bytes[((y.rem_euclid(4) * 4 + x.rem_euclid(4)) * 4) as usize + c] as f32 / 255.
        })
    };
    (0..48)
        .flat_map(|y| (0..64).map(move |x| (x, y)))
        .map(|(x, y)| {
            let mut at = [(x as f64 + 0.5) / scale, (y as f64 + 0.5) / scale];
            if let Some(crop) = crop {
                at = [
                    crop.x0 + at[0].rem_euclid(crop.width()),
                    crop.y0 + at[1].rem_euclid(crop.height()),
                ];
                at = at.map(|v| v.clamp(0.5, 3.5));
            }
            if quality == ImageQuality::Low {
                return texel(at[0].floor() as i32, at[1].floor() as i32);
            }
            let q = at.map(|v| v - 0.5);
            let base = q.map(|v| v.floor() as i32);
            let t = q.map(|v| (v - v.floor()) as f32);
            let a = texel(base[0], base[1]);
            let b = texel(base[0] + 1, base[1]);
            let c = texel(base[0], base[1] + 1);
            let d = texel(base[0] + 1, base[1] + 1);
            std::array::from_fn(|i| {
                (a[i] * (1. - t[0]) + b[i] * t[0]) * (1. - t[1])
                    + (c[i] * (1. - t[0]) + d[i] * t[0]) * t[1]
            })
        })
        .collect()
}

fn panned_image(lease: ResourceLease, pool: &BudgetPool) -> RenderFrame {
    let mut widget = r01_fixture::RecordWidget::new(reel_ui::imaging::record::Scene::new());
    widget.clip = false;
    widget.resource_draws.push((
        lease.clone(),
        Rect::new(0., 0., 4., 4.),
        Affine::translate((-200., -200.)) * Affine::scale(200.),
        reel_ui::peniko::ImageSampler {
            alpha: 0.5,
            ..Default::default()
        },
    ));
    let mut bridge = r01_fixture::widget_bridge(widget, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(3)).unwrap(),
        ResourceSnapshot::capture([Ok(lease)].into_iter(), pool).unwrap(),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        r01_fixture::limits(),
        pool,
    )
    .unwrap();
    while !build.step(r01_fixture::step_budget()).unwrap().complete {}
    build.finish().unwrap()
}

#[test]
#[ignore = "actual GPU numeric sampling; no visual acceptance"]
fn atlas_repeat_seams_identity_alias_and_direct_paths_match_their_numeric_contract() {
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 128,
            max_bytes: 8 << 20,
        },
    );
    let mut registry = ResourceRegistry::new(4, 1 << 20).unwrap();
    let pixels: Vec<u8> = (0..4)
        .flat_map(|y| (0..4).flat_map(move |x| [x * 60, y * 60, 240 - x * 40, 255]))
        .collect();
    let mut page = [0, 255, 0, 255].repeat(12 * 12);
    for y in 0..4 {
        for x in 0..4 {
            page[((y + 4) * 12 + x + 4) * 4..((y + 4) * 12 + x + 4) * 4 + 4]
                .copy_from_slice(&pixels[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4]);
        }
    }
    let version = |name: &str| ResourceVersion {
        key: AssetKey(name.into()),
        version: ContentVersion([9; 32]),
    };
    let upload = |registry: &mut ResourceRegistry, name: &str, size, data: Vec<u8>| {
        let ticket = pool.reserve(data.capacity() as u64).unwrap();
        registry
            .register_texture(
                version(name),
                TexturePayload::new(
                    TextureDescriptor {
                        storage_layout: Default::default(),
                        mip_count: 1,
                        format: TextureFormat::Rgba8,
                        source_encoding: ColorEncoding::Linear,
                        layout: TextureLayout::identity(size),
                    },
                    OwnedBytes::new(data, ticket).unwrap(),
                )
                .unwrap(),
            )
            .unwrap()
    };
    let standalone = upload(&mut registry, "single", [4, 4], pixels.clone());
    let page = upload(&mut registry, "page", [12, 12], page);
    let alias = registry
        .register_atlas_view(
            version("view"),
            ImageView {
                logical_size: [4, 4],
                content_origin: [4, 4],
                content_size: [4, 4],
            },
            ColorEncoding::Linear,
            registry.acquire(page).unwrap(),
        )
        .unwrap();
    let identity = registry
        .register_atlas_view(
            version("identity"),
            ImageView::from_layout(TextureLayout::identity([4, 4])),
            ColorEncoding::Linear,
            registry.acquire(standalone).unwrap(),
        )
        .unwrap();
    let mut renderer = Renderer::new(
        pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            DeviceGeneration(1),
            GpuLimits::default(),
        ))
        .unwrap(),
    );
    let single = panned_image(registry.acquire(standalone).unwrap(), &pool);
    let panned = panned_image(registry.acquire(alias).unwrap(), &pool);
    let prepared = renderer.prepare(&single).unwrap();
    let draws = renderer
        .pipelines
        .get(&renderer.gpu, prepared.quality)
        .unwrap();
    let single = renderer.readback(prepared).unwrap();
    let before_direct =
        draws.direct_draws[crate::common::mesh::DirectPaint::Rgba as usize].load(Ordering::Relaxed);
    let prepared = renderer.prepare(&panned).unwrap();
    let panned = renderer.readback(prepared).unwrap();
    assert!(
        draws.direct_draws[crate::common::mesh::DirectPaint::Rgba as usize].load(Ordering::Relaxed)
            > before_direct,
        "the visible interior of the offscreen atlas quad selects whole-texture sampling"
    );
    let mut oracle_error = 0f32;
    for y in 0..300 {
        for x in 0..400 {
            let at = [
                (x as f64 + 0.5 + 200.) / 200. - 0.5,
                (y as f64 + 0.5 + 200.) / 200. - 0.5,
            ];
            let base = at.map(|v| v.floor() as usize);
            let t = at.map(|v| v - v.floor());
            for channel in 0..4 {
                let texel = |dx: usize, dy: usize| {
                    f64::from(pixels[((base[1] + dy) * 4 + base[0] + dx) * 4 + channel]) / 255.
                };
                let expected = ((texel(0, 0) * (1. - t[0]) + texel(1, 0) * t[0]) * (1. - t[1])
                    + (texel(0, 1) * (1. - t[0]) + texel(1, 1) * t[0]) * t[1])
                    * 0.5;
                oracle_error =
                    oracle_error.max((panned[y * 400 + x][channel] - expected as f32).abs());
            }
        }
    }
    assert!(error(&single, &panned) < 0.002);
    assert!(
        oracle_error < 0.002,
        "visible atlas interior numeric oracle: {oracle_error}"
    );
    eprintln!("offscreen atlas direct-sampling max error={oracle_error}");
    for (view, scale, quality, crop) in [
        (alias, 1.75, ImageQuality::Medium, None),
        (
            alias,
            1.75,
            ImageQuality::Medium,
            Some(Rect::new(-2., -1., 6., 7.)),
        ),
        // No pixel center falls exactly on a nearest texel boundary.
        (identity, 2.37, ImageQuality::Low, None),
    ] {
        let a = pattern(
            registry.acquire(standalone).unwrap(),
            scale,
            quality,
            crop,
            &pool,
        );
        let b = pattern(registry.acquire(view).unwrap(), scale, quality, crop, &pool);
        let a = renderer.readback(renderer.prepare(&a).unwrap()).unwrap();
        let b = renderer.readback(renderer.prepare(&b).unwrap()).unwrap();
        let expected = oracle(&pixels, scale, quality, crop);
        let oracle_error = error(&a, &expected);
        let view_error = error(&a, &b);
        assert!(
            oracle_error < 0.012,
            "standalone oracle {scale} {quality:?} {crop:?}: {oracle_error}"
        );
        assert!(
            view_error < 0.012,
            "atlas {scale} {quality:?} {crop:?}: {view_error}"
        );
        eprintln!(
            "atlas sample scale={scale} quality={quality:?} crop={crop:?} oracle_error={oracle_error} view_error={view_error}"
        );
    }
    let before = renderer
        .gpu
        .upload_creations
        .appearance_buffers
        .load(Ordering::Relaxed);
    let ordinary = frame(registry.acquire(alias).unwrap(), 0, &pool);
    let control = frame(registry.acquire(standalone).unwrap(), 0, &pool);
    let prepared = renderer.prepare(&ordinary).unwrap();
    let draws = renderer
        .pipelines
        .get(&renderer.gpu, prepared.quality)
        .unwrap();
    let a = renderer.readback(prepared).unwrap();
    let b = renderer
        .readback(renderer.prepare(&control).unwrap())
        .unwrap();
    assert!(error(&a, &b) < 0.012);
    assert!(
        draws.direct_draws[crate::common::mesh::DirectPaint::RegionalRgba as usize]
            .load(Ordering::Relaxed)
            > 0,
        "the preceding repeat cases cannot select a direct pipeline"
    );
    assert_eq!(
        renderer
            .gpu
            .upload_creations
            .appearance_buffers
            .load(Ordering::Relaxed),
        before
    );
    let mut widget = r01_fixture::RecordWidget::new(reel_ui::imaging::record::Scene::new());
    widget.clip = false;
    widget.sprite_draws.push((
        registry.acquire(alias).unwrap(),
        Rect::new(0., 0., 4., 4.),
        Affine::IDENTITY,
        SpriteInstances::try_collect(&pool, |emit| {
            for x in [40., 80., 120.] {
                emit(SpriteInstance {
                    position: [x, 40.],
                    size: 24.,
                    rotation: 0.17,
                    alpha: 1.,
                });
            }
        }),
    ));
    let mut bridge = r01_fixture::widget_bridge(widget, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(2)).unwrap(),
        registry.freeze(&[alias], &pool).unwrap(),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        r01_fixture::limits(),
        &pool,
    )
    .unwrap();
    while !build.step(r01_fixture::step_budget()).unwrap().complete {}
    let frame = build.finish().unwrap();
    let prepared = renderer.prepare(&frame).unwrap();
    assert!(
        prepared
            .geometry
            .iter()
            .any(|g| g.storage.instances.is_some())
    );
    assert_eq!(size_of::<crate::common::quad_instances::Instance>(), 48);
    renderer.submit(prepared).unwrap();
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
}
