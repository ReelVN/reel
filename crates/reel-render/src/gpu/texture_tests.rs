//! Actual-device submission, allocation and authored-coordinate contracts; no readback.
use super::super::{DeviceGeneration, GpuLimits, resources};
use super::*;
use reel_core::{
    budget::{BudgetCategory, OwnedBytes, QueueLimits},
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion, SurfaceEpoch},
    texture::*,
};
use reel_ui::{
    app::{GeometryLimits, RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::{BridgeLimits, PresentationBridge},
    core::*,
    dpi::PhysicalSize,
    frame::{BuildLimits, FrameBuild, FrameInfo, ImageAlphaLayout, StepBudget},
    kurbo::{Insets, Rect},
    resources::{ResourceLease, ResourceRegistry, ResourceSnapshot},
    widgets::Image,
};
#[allow(dead_code, reason = "shared actual UI fixture")]
#[path = "../../../reel-ui/tests/support/fixture.rs"]
mod fixture;
#[allow(dead_code, reason = "shared actual UI recording fixture")]
#[path = "../../../reel-ui/tests/support/r01_fixture.rs"]
mod r01_fixture;

fn frame(lease: ResourceLease, variant: u32, pool: &BudgetPool) -> RenderFrame {
    let mut bridge = if matches!(variant, 5 | 8) {
        use reel_ui::{
            imaging::Composite,
            imaging::record::{Draw, Geometry, Scene},
            kurbo::Affine,
            peniko::{Brush, Color},
        };
        let mut scene = Scene::new();
        if variant == 5 {
            scene.draw(Draw::Fill {
                transform: Affine::IDENTITY,
                fill_rule: Default::default(),
                brush: Brush::Solid(Color::WHITE),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(0., 0., 64., 48.)),
                composite: Composite::default(),
            });
        }
        let mut widget = r01_fixture::RecordWidget::new(scene);
        widget.clip = false;
        if variant == 5 {
            widget.mask = Some(PaintImageMask {
                program: reel_ui::stage::MaskProgram::image("mask".into()),
                images: vec![lease.clone()].into(),
                bounds: Rect::new(0., 0., 64., 48.),
                parameters: [0.5, 0., 0., 0., 0.],
            });
        } else {
            for alpha in [1., 0.5, 1.] {
                widget.resource_draws.push((
                    lease.clone(),
                    Rect::new(0., 0., 64., 48.),
                    Affine::IDENTITY,
                    reel_ui::peniko::ImageSampler {
                        alpha,
                        ..Default::default()
                    },
                ));
            }
        }
        r01_fixture::widget_bridge(widget, Affine::IDENTITY)
    } else {
        let mut widget = Image::from_resource(lease.clone());
        widget = match variant {
            1 => widget.with_source_rect(Some(Rect::new(0., 0., 24., 16.))),
            2 => widget.with_slice(Some(Insets::uniform(4.)), 1., true),
            3 => widget.with_pattern([true, true], 1., None),
            4 => widget.with_alpha_layout(ImageAlphaLayout::TopBottom),
            6 => widget.with_color(reel_ui::peniko::Color::from_rgba8(255, 255, 255, 128)),
            7 => widget
                .with_source_rect(Some(Rect::new(0., 0., 24., 16.)))
                .with_color(reel_ui::peniko::Color::from_rgba8(255, 255, 255, 128)),
            _ => widget,
        };
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
        PresentationBridge::new(
            root,
            SurfaceEpoch(1),
            BridgeLimits {
                geometry: GeometryLimits { max_bytes: 1 << 20 },
                max_candidates: 2,
                max_bridge_bytes: 65536,
                max_snapshot_bytes: 4 << 20,
            },
        )
        .unwrap()
    };
    let mut build = FrameBuild::new(
        bridge.record(FrameId(u64::from(variant) + 1)).unwrap(),
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

#[test]
#[ignore = "requires actual BC-capable graphics adapter; submits without pixel readback"]
fn direct_paint_submits_whole_and_regional_alpha_policies() {
    use std::sync::atomic::Ordering;
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 128,
            max_bytes: 8 << 20,
        },
    );
    let mut registry = ResourceRegistry::new(3, 65536).unwrap();
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        GpuLimits::default(),
    ))
    .unwrap();
    assert!(
        gpu.device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    );
    let mut renderer = Renderer::new(gpu);
    let mut expected = [0; 8];
    let mut opaque = [0; 3];
    for (index, format) in [
        TextureFormat::Rgba8,
        TextureFormat::Bc1Rgb,
        TextureFormat::Bc3Rgba,
    ]
    .into_iter()
    .enumerate()
    {
        let descriptor = TextureDescriptor {
            format,
            source_encoding: ColorEncoding::Linear,
            layout: TextureLayout::identity([32, 32]),
        };
        let bytes = descriptor.byte_len().unwrap();
        let handle = registry
            .register_texture(
                ResourceVersion {
                    key: AssetKey(format!("direct-paint-{index}")),
                    version: ContentVersion([index as u8; 32]),
                },
                TexturePayload::new(
                    descriptor,
                    OwnedBytes::new(vec![0; bytes as usize], pool.reserve(bytes).unwrap()).unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
        for variant in [0, 1, 6, 7, 3, 4, 8] {
            if variant == 8 && format != TextureFormat::Bc1Rgb {
                continue;
            }
            let frame = frame(registry.acquire(handle).unwrap(), variant, &pool);
            let prepared = renderer.prepare(&frame).unwrap();
            let pipelines = renderer
                .pipelines
                .get(&renderer.gpu, prepared.quality())
                .unwrap();
            if matches!(variant, 0 | 6 | 8) {
                expected[index] += 1;
            } else if matches!(variant, 1 | 7) {
                expected[index + 3] += 1;
            }
            if format == TextureFormat::Bc1Rgb && variant <= 1 {
                opaque[variant as usize] += 1;
            }
            assert_eq!(
                pipelines
                    .direct_draws
                    .each_ref()
                    .map(|count| count.load(Ordering::Relaxed)),
                expected,
                "format {format:?}, variant {variant}",
            );
            assert_eq!(
                pipelines
                    .opaque_draws
                    .each_ref()
                    .map(|count| count.load(Ordering::Relaxed)),
                opaque,
                "only opaque BC1 uses replacement: format {format:?}, variant {variant}"
            );
            renderer.submit(prepared).unwrap();
            renderer
                .gpu
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            renderer.poll().unwrap();
        }
    }
    assert_eq!(expected, [2, 3, 2, 2, 2, 2, 0, 0]);
    assert_eq!(opaque, [1, 1, 0]);
    assert_eq!(renderer.pipelines.creations.snapshot(), [3, 18, 18, 18, 8]);
    assert_eq!(renderer.usage().transient, renderer.usage().cached_geometry);
}

#[test]
#[ignore = "requires actual BC-capable graphics adapter"]
fn typed_tiles_submit_mixed_raster_bc1_bc3_resources_with_exact_leases() {
    use reel_ui::{
        frame::{FrameDraw, FrameOp},
        kurbo::{Affine, Size},
        peniko::{Brush, ImageAlphaType, ImageFormat},
        widgets::ImageTile,
    };
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 128,
            max_bytes: 8 << 20,
        },
    );
    let mut registry = ResourceRegistry::new(3, 65536).unwrap();
    let mut leases = Vec::new();
    for (index, format) in [
        TextureFormat::Rgba8,
        TextureFormat::Bc1Rgb,
        TextureFormat::Bc3Rgba,
    ]
    .into_iter()
    .enumerate()
    {
        let version = ResourceVersion {
            key: AssetKey(format!("tile-{index}")),
            version: ContentVersion([index as u8; 32]),
        };
        let handle = if index == 0 {
            registry
                .register_image(
                    version,
                    OwnedBytes::new(vec![0; 18 * 10 * 4], pool.reserve(18 * 10 * 4).unwrap())
                        .unwrap(),
                    [18, 10],
                    ImageFormat::Rgba8,
                    ImageAlphaType::Alpha,
                    ColorEncoding::Srgb,
                )
                .unwrap()
        } else {
            let descriptor = TextureDescriptor {
                format,
                source_encoding: ColorEncoding::Srgb,
                layout: TextureLayout {
                    logical_size: [18, 10],
                    content_size: [9, 5],
                    storage_size: [12, 8],
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
        };
        leases.push(registry.acquire(handle).unwrap());
    }
    let tiles = leases
        .iter()
        .enumerate()
        .map(|(i, resource)| ImageTile {
            resource: resource.clone(),
            transform: Affine::translate((i as f64 * 16. - 1., -1.)),
            clip: Rect::new(i as f64 * 16., 0., (i + 1) as f64 * 16., 8.),
        })
        .collect();
    let root = RenderRoot::new(
        Image::tiled(Size::new(48., 8.), tiles).prepare(),
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(96, 48),
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
        ResourceSnapshot::capture(leases.iter().cloned().map(Ok), &pool).unwrap(),
        FrameInfo {
            logical_size: [96., 48.],
            physical_size: [96, 48],
        },
        BuildLimits { max_bytes: 1 << 20 },
        &pool,
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
    let frame = build.finish().unwrap();
    let images: Vec<_> = frame
        .commands()
        .iter()
        .filter_map(|c| match &c.op {
            FrameOp::Draw(FrameDraw::Fill {
                brush: Brush::Image(image),
                brush_transform,
                ..
            }) => Some((image.image.resource, *brush_transform)),
            _ => None,
        })
        .collect();
    assert_eq!(images.len(), 3);
    for (i, (handle, sampling)) in images.into_iter().enumerate() {
        assert_eq!(handle, leases[i].handle());
        assert_eq!(sampling, Some(Affine::translate((1. - i as f64 * 16., 1.))));
    }
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        GpuLimits::default(),
    ))
    .unwrap();
    eprintln!("typed tile submission adapter={:?}", gpu.adapter.get_info());
    let mut renderer = Renderer::new(gpu);
    let prepared = renderer.prepare(&frame).unwrap();
    for (index, lease) in leases.iter().enumerate() {
        let image = &prepared.images[&lease.handle()];
        assert_eq!(
            image.texture.format(),
            match index {
                0 => wgpu::TextureFormat::Rgba16Float,
                1 => wgpu::TextureFormat::Bc1RgbaUnorm,
                _ => wgpu::TextureFormat::Bc3RgbaUnorm,
            }
        );
        assert_eq!(image.layout.logical_size, [18, 10]);
    }
    let versions: Vec<_> = leases.iter().map(|l| l.version().clone()).collect();
    let id = renderer.submit(prepared).unwrap();
    drop(frame);
    drop(bridge);
    for lease in &leases {
        registry.retire(lease.handle());
    }
    drop(leases);
    drop(registry);
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert!(renderer.poll().unwrap().contains(&id));
    renderer.retire(&versions);
    assert_eq!(
        pool.usage().bytes,
        0,
        "submitted tiles retain their real storage through completion"
    );
}
#[test]
#[ignore = "requires actual BC-capable graphics adapter"]
fn prepared_bc_textures_submit_exact_storage_and_preserve_sampling_metadata() {
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 512,
            max_bytes: 8 << 20,
        },
    );
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        GpuLimits {
            image_upload_bytes_per_prepare: 256,
            ..Default::default()
        },
    ))
    .unwrap();
    assert!(
        gpu.device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    );
    let mut renderer = Renderer::new(gpu);
    for (format_index, format) in [
        TextureFormat::Bc1Rgb,
        TextureFormat::Bc3Rgba,
        TextureFormat::Rgba8,
    ]
    .into_iter()
    .enumerate()
    {
        let descriptor = TextureDescriptor {
            format,
            source_encoding: ColorEncoding::Srgb,
            layout: TextureLayout {
                logical_size: [48, 32],
                content_size: [12, 8],
                storage_size: [16, 16],
            },
        };
        let length = descriptor.byte_len().unwrap();
        let bytes =
            OwnedBytes::new(vec![0; length as usize], pool.reserve(length).unwrap()).unwrap();
        let version = ResourceVersion {
            key: AssetKey(format!("blocks-{format_index}")),
            version: ContentVersion([format_index as u8; 32]),
        };
        let mut registry = ResourceRegistry::new(1, 65536).unwrap();
        let handle = registry
            .register_texture(
                version.clone(),
                TexturePayload::new(descriptor, bytes).unwrap(),
            )
            .unwrap();
        let lease = registry.acquire(handle).unwrap();
        let expected_layout = TextureLayout {
            storage_size: if format == TextureFormat::Rgba8 {
                [16, 8]
            } else {
                [16, 16]
            },
            ..descriptor.layout
        };
        let resident = format.byte_len(expected_layout.storage_size).unwrap();
        assert_eq!(
            resources::texture_costs(
                crate::common::resources::TextureUploadLayout::new(descriptor).unwrap()
            ),
            (
                resident,
                256 * u64::from(expected_layout.storage_size[1] / format.block_size())
            )
        );
        for variant in 0..6 {
            let frame = frame(lease.clone(), variant, &pool);
            let appearances = renderer
                .gpu
                .upload_creations
                .appearance_buffers
                .load(std::sync::atomic::Ordering::Relaxed);
            let prepared = loop {
                match renderer.prepare(&frame) {
                    Ok(p) => break p,
                    Err(error) if error.is_retryable() => {
                        renderer
                            .gpu
                            .device
                            .poll(wgpu::PollType::wait_indefinitely())
                            .unwrap();
                        renderer.poll().unwrap();
                    }
                    Err(e) => panic!("prepare failed {e:?}"),
                }
            };
            let image = &prepared.images[&handle];
            assert_eq!(image.texture.format(), resources::texture_format(format));
            assert_eq!(image.layout, expected_layout);
            assert_eq!(image.residency.as_ref().unwrap().bytes(), resident);
            assert_eq!(image.size, expected_layout.storage_size);
            assert_eq!(
                renderer
                    .gpu
                    .upload_creations
                    .appearance_buffers
                    .load(std::sync::atomic::Ordering::Relaxed)
                    - appearances,
                u64::from(variant == 4) as usize,
                "plain prepared textures reuse fixed appearance buffers"
            );
            let id = renderer.submit(prepared).unwrap();
            renderer
                .gpu
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            assert!(renderer.poll().unwrap().contains(&id));
        }
        renderer.retire(&[version]);
        drop(lease);
        drop(registry);
    }
    assert_eq!(
        pool.usage().bytes,
        0,
        "frames/uploads/resources must release their CPU reservations"
    );
}
