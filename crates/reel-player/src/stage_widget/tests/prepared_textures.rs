//! Production particle paint with typed raster/BC resources; no pixel readback.
use super::*;
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, ResourceVersion},
    texture::*,
};
use reel_ui::{
    frame::{BuildLimits, FrameBuild, FrameDraw, FrameInfo, FrameOp, RenderFrame, StepBudget},
    peniko::{Brush, ImageAlphaType, ImageFormat},
    resources::{ResourceRegistry, ResourceSnapshot},
    stage::particles::*,
};

fn particle_frame(compressed: bool) -> (RenderFrame, BudgetPool) {
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 128,
            max_bytes: 16 << 20,
        },
    );
    let mut registry = ResourceRegistry::new(1, 65536).unwrap();
    let version = ResourceVersion {
        key: AssetKey("sprite".into()),
        version: ContentVersion([u8::from(compressed); 32]),
    };
    let handle = if compressed {
        let descriptor = TextureDescriptor {
            storage_layout: reel_core::texture::TextureStorageLayout::Linear,
            mip_count: 1,
            format: TextureFormat::Bc3Rgba,
            source_encoding: ColorEncoding::Srgb,
            layout: TextureLayout {
                logical_size: [48, 24],
                content_size: [6, 3],
                storage_size: [8, 4],
            },
        };
        let length = descriptor.byte_len().unwrap();
        registry
            .register_texture(
                version,
                TexturePayload::new(
                    descriptor,
                    OwnedBytes::new(vec![0; length as usize], pool.reserve(length).unwrap())
                        .unwrap(),
                )
                .unwrap(),
            )
            .unwrap()
    } else {
        registry
            .register_image(
                version,
                OwnedBytes::new(vec![255; 48 * 24 * 4], pool.reserve(48 * 24 * 4).unwrap())
                    .unwrap(),
                [48, 24],
                ImageFormat::Rgba8,
                ImageAlphaType::Alpha,
                ColorEncoding::Srgb,
            )
            .unwrap()
    };
    let batch = ParticleNode {
        source: "sprite".into(),
        origin: Some("particles.aside:8:3".into()),
        seed: 42,
        size_scale: 1.,
        time: 0.,
        batches: ParticleBatches::try_new(
            &pool,
            1,
            [ParticleEmission {
                sequence: 0,
                config: ParticleConfig {
                    rate: 6.,
                    burst: 2,
                    lifetime: 2.,
                    size: 24.,
                    opacity: [0.6, 0.6],
                    lifetime_spread: 0.,
                    seed: 42,
                    velocity: [5., 8.],
                    spread: [2., 3.],
                    acceleration: [0., 0.],
                    size_spread: 0.,
                    size_end: None,
                    size_curve: None,
                    angle: 0.,
                    angle_spread: 0.,
                    radial_speed: 0.,
                    radial_spread: 0.,
                    direction: 0.,
                    direction_spread: 180.,
                    damping: 0.,
                    spin: 10.,
                    spin_spread: 0.,
                },
                frame: ParticleFrame {
                    origin: [20., 20.],
                    elapsed: 0.5,
                    duration: None,
                    extent: [100., 60.],
                },
                started: 0.,
                paused: false,
            }],
        )
        .unwrap(),
    };
    let samples: Vec<_> = batch.samples().collect();
    assert!(!samples.is_empty());
    let mut stage = Stage::default();
    let id = stage
        .create(
            NodeKind::Particles(batch),
            NodeProps {
                width: Some(200.),
                height: Some(120.),
                background: Some([25, 50, 75, 255]),
                border_width: 2.,
                border_color: [175, 125, 50, 255],
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    stage.replace(id).unwrap();
    let mut view = StageView::capture(&stage);
    view.resolve_images(
        &BTreeMap::from([("sprite".into(), registry.acquire(handle).unwrap())]),
        EffectShaderFormat::Wgsl,
    )
    .unwrap();
    let widget = NewWidget::new(StageWidget::new(Some(&view)));
    let root = RenderRoot::new(
        widget,
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(640, 420),
            scale_factor: 1.,
            default_font: None,
        },
    );
    let mut bridge = PresentationBridge::new(
        root,
        SurfaceEpoch(1),
        BridgeLimits {
            geometry: GeometryLimits { max_bytes: 4 << 20 },
            max_candidates: 4,
            max_bridge_bytes: 65536,
            max_snapshot_bytes: 24 << 20,
        },
    )
    .unwrap();
    let source = bridge.record(FrameId(1)).unwrap();
    let snapshot =
        ResourceSnapshot::capture(view.resources.iter().cloned().map(Ok), &pool).unwrap();
    let mut build = FrameBuild::new(
        source,
        snapshot,
        FrameInfo {
            logical_size: [640., 420.],
            physical_size: [640, 420],
        },
        BuildLimits { max_bytes: 4 << 20 },
        &pool,
    )
    .unwrap();
    while !build
        .step(StepBudget {
            max_commands: 64,
            max_units: 65536,
            max_bytes: 1 << 20,
        })
        .unwrap()
        .complete
    {}
    let frame = build.finish().unwrap();
    let batches: Vec<_> = frame
        .commands()
        .iter()
        .filter_map(|c| match &c.op {
            FrameOp::Draw(FrameDraw::Sprites(batch)) => Some(batch),
            _ => None,
        })
        .collect();
    assert_eq!(batches.len(), 1);
    let batch = batches[0];
    assert_eq!(batch.instances.len(), samples.len());
    assert!(
        frame
            .commands()
            .iter()
            .filter(|c| matches!(
                &c.op,
                FrameOp::Draw(FrameDraw::Fill {
                    brush: Brush::Solid(_),
                    ..
                }) | FrameOp::Draw(FrameDraw::Stroke {
                    brush: Brush::Solid(_),
                    ..
                })
            ))
            .count()
            >= 2,
        "particle background and border keep their original brushes"
    );
    for (instance, sample) in batch.instances.iter().zip(samples) {
        let brush = &batch.brush;
        let transform = batch.transform * instance.transform(batch.bounds);
        assert_eq!(brush.image.resource, handle);
        assert_eq!(brush.sampler.alpha, 1.);
        assert!((instance.alpha - sample.opacity as f32).abs() < 1e-6);
        let expected = Affine::translate((sample.position[0], sample.position[1]))
            * Affine::rotate(sample.rotation.to_radians())
            * Affine::scale(sample.size / 48.)
            * Affine::translate((-24., -12.));
        for (actual, expected) in transform.as_coeffs().into_iter().zip(expected.as_coeffs()) {
            assert!(
                (actual - expected).abs() < 1e-8,
                "particle transform uses logical source dimensions"
            );
        }
    }
    registry.retire(handle);
    (frame, pool)
}

#[test]
fn particle_resource_draws_preserve_background_alpha_geometry_and_lifetime() {
    for compressed in [false, true] {
        let (frame, pool) = particle_frame(compressed);
        assert!(pool.usage().bytes > 0);
        drop(frame);
        assert_eq!(pool.usage().bytes, 0);
    }
}

#[cfg(feature = "window")]
#[test]
#[ignore = "requires actual BC-capable graphics adapter"]
fn particle_raster_and_bc_resources_submit_without_expansion() {
    use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        GpuLimits::default(),
    ))
    .unwrap();
    eprintln!("particle submission adapter={:?}", gpu.adapter.get_info());
    let device = gpu.device.clone();
    let mut renderer = Renderer::new(gpu);
    for compressed in [false, true] {
        let (frame, pool) = particle_frame(compressed);
        assert_eq!(
            matches!(
                frame.resources().leases()[0].view(),
                reel_ui::resources::ResourceView::Texture { .. }
            ),
            compressed
        );
        let prepared = renderer.prepare(&frame).unwrap();
        let id = renderer.submit(prepared).unwrap();
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        assert!(renderer.poll().unwrap().contains(&id));
        drop(frame);
        assert_eq!(pool.usage().bytes, 0);
    }
}
