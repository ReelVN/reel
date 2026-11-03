//! Numeric output checks for constant RGB adjustment and independent alpha.
use super::*;
use reel_ui::{frame::ImageColorAdjustment, peniko::Color};

fn adjusted_frame(
    lease: ResourceLease,
    pool: &BudgetPool,
    adjustment: ImageColorAdjustment,
    packed: bool,
    crop: Option<Rect>,
) -> RenderFrame {
    let widget = Image::from_resource(lease.clone())
        .with_color(Color::from_rgba8(210, 150, 90, 128))
        .with_color_adjustment(adjustment)
        .with_alpha_layout(if packed {
            ImageAlphaLayout::TopBottom
        } else {
            ImageAlphaLayout::Native
        })
        .with_source_rect(crop);
    let mut bridge = PresentationBridge::new(
        RenderRoot::new(
            widget.prepare(),
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(16, 8),
                scale_factor: 1.,
                default_font: None,
            },
        ),
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
            logical_size: [16., 8.],
            physical_size: [16, 8],
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

#[test]
#[ignore = "actual GPU readback against independent f64 color and alpha oracle"]
fn zero_gain_preserves_filtered_alpha_for_native_and_packed_images() {
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 128,
            max_bytes: 8 << 20,
        },
    );
    let mut registry = ResourceRegistry::new(4, 1 << 20).unwrap();
    let bytes = vec![
        0, 0, 0, 0, 48, 16, 32, 64, 16, 100, 48, 128, 200, 160, 80, 255, 0, 0, 0, 255, 64, 64, 64,
        255, 128, 128, 128, 255, 255, 255, 255, 255,
    ];
    let ticket = pool.reserve(bytes.capacity() as u64).unwrap();
    let handle = registry
        .register_texture(
            ResourceVersion {
                key: AssetKey("constant-adjustment".into()),
                version: ContentVersion([7; 32]),
            },
            TexturePayload::new(
                TextureDescriptor {
                    storage_layout: Default::default(),
                    mip_count: 1,
                    format: TextureFormat::Rgba8,
                    source_encoding: ColorEncoding::Linear,
                    layout: TextureLayout::identity([4, 2]),
                },
                OwnedBytes::new(bytes, ticket).unwrap(),
            )
            .unwrap(),
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
    for packed in [false, true] {
        for crop in [None, Some(Rect::new(-1., 0., 5., 1.))] {
            let control = adjusted_frame(
                registry.acquire(handle).unwrap(),
                &pool,
                ImageColorAdjustment::IDENTITY,
                packed,
                crop,
            );
            let control = renderer
                .readback(renderer.prepare(&control).unwrap())
                .unwrap();
            assert!(control.iter().any(|p| p[3] > 0.1 && p[3] < 0.4));
            for offset in [[-0.2, 0.03, 1.2], [0.37, 0.62, 0.9]] {
                let frame = adjusted_frame(
                    registry.acquire(handle).unwrap(),
                    &pool,
                    ImageColorAdjustment {
                        saturation: 3.25,
                        gain: [0.; 3],
                        offset,
                    },
                    packed,
                    crop,
                );
                let pixels = renderer
                    .readback(renderer.prepare(&frame).unwrap())
                    .unwrap();
                let linear = offset.map(|value| {
                    let value = f64::from(value).clamp(0., 1.);
                    if value <= 0.04045 {
                        value / 12.92
                    } else {
                        ((value + 0.055) / 1.055).powf(2.4)
                    }
                });
                for (actual, base) in pixels.iter().zip(&control) {
                    assert!(
                        (actual[3] - base[3]).abs() < 0.005,
                        "alpha {packed} {crop:?}: {actual:?} vs {base:?}"
                    );
                    for c in 0..3 {
                        let expected = linear[c] * f64::from(base[3]);
                        assert!(
                            (f64::from(actual[c]) - expected).abs() < 0.007,
                            "RGB {packed} {crop:?} offset={offset:?}: {actual:?} vs alpha={} channel={c} expected={expected}",
                            base[3]
                        );
                    }
                }
            }
        }
    }
}
