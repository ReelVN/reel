//! Actual-device submission, allocation and authored-coordinate contracts.
//! The separate atlas sampling module adds finite numeric readback checks.
#[path = "atlas_sampling_tests.rs"]
mod atlas_sampling_tests;
#[path = "image_adjustment_tests.rs"]
mod image_adjustment_tests;
use super::super::{DeviceGeneration, GpuLimits, resources};
use super::*;
use crate::test_support::r01_fixture;
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

fn frame(lease: ResourceLease, variant: u32, pool: &BudgetPool) -> RenderFrame {
    if variant == 10 {
        return effect_frame(lease, pool);
    }
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
            9 => widget
                .with_source_rect(Some(Rect::new(-2., -1., 6., 7.)))
                .with_pattern([true, true], 1., None),
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
            storage_layout: reel_core::texture::TextureStorageLayout::Linear,
            mip_count: 1,
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
                storage_layout: reel_core::texture::TextureStorageLayout::Linear,
                mip_count: 1,
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
            storage_layout: reel_core::texture::TextureStorageLayout::Linear,
            mip_count: 1,
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

#[test]
#[ignore = "requires actual BC-capable graphics adapter; submits without pixel readback"]
fn retired_texture_storage_reuses_exact_footprints_and_preserves_reader_pins() {
    use std::sync::atomic::Ordering;
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
        GpuLimits::default(),
    ))
    .unwrap();
    assert!(
        gpu.device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    );
    let mut renderer = Renderer::new(gpu);
    for (index, format) in [
        TextureFormat::Rgba8,
        TextureFormat::Bgra8,
        TextureFormat::LinearRgba16Float,
        TextureFormat::Bc1Rgb,
        TextureFormat::Bc3Rgba,
    ]
    .into_iter()
    .enumerate()
    {
        let start = renderer
            .gpu
            .upload_creations
            .textures
            .load(Ordering::Relaxed);
        let mut identity = None;
        let mut reader = None;
        let mut cpu_lease = None;
        for version in 0..10u8 {
            let storage_size = if version == 8 { [8, 16] } else { [8, 8] };
            let descriptor = TextureDescriptor {
                storage_layout: reel_core::texture::TextureStorageLayout::Linear,
                mip_count: 1,
                format,
                source_encoding: if version % 2 == 0 {
                    ColorEncoding::Srgb
                } else {
                    ColorEncoding::Linear
                },
                layout: TextureLayout {
                    logical_size: [32 + u32::from(version), 24],
                    content_size: storage_size,
                    storage_size,
                },
            };
            let length = descriptor.byte_len().unwrap();
            let mut pixels = vec![255; length as usize];
            if format == TextureFormat::LinearRgba16Float {
                for sample in pixels.as_chunks_mut::<2>().0 {
                    sample.copy_from_slice(&0x3c00u16.to_le_bytes());
                }
            }
            let bytes = OwnedBytes::new(pixels, pool.reserve(length).unwrap()).unwrap();
            let resource = ResourceVersion {
                key: AssetKey(format!("reuse-{index}")),
                version: ContentVersion([version; 32]),
            };
            let mut registry = ResourceRegistry::new(1, 65536).unwrap();
            let handle = registry
                .register_texture(
                    resource.clone(),
                    TexturePayload::new(descriptor, bytes).unwrap(),
                )
                .unwrap();
            let lease = registry.acquire(handle).unwrap();
            if index == 4 && version == 1 {
                cpu_lease = Some(lease.clone());
            }
            let frame = frame(lease, 0, &pool);
            let prepared = renderer.prepare(&frame).unwrap();
            let image = &prepared.images[&handle];
            assert_eq!(image.layout, descriptor.layout);
            assert_eq!(image.encoding, descriptor.source_encoding);
            assert_eq!(image.residency.as_ref().unwrap().bytes(), length);
            assert_eq!(image.texture.format(), resources::texture_format(format));
            if index == 4 && version == 0 {
                reader = Some(image.clone());
            }
            if index != 4 && version != 8 {
                if let Some(texture) = &identity {
                    assert_eq!(texture, &image.texture);
                } else {
                    identity = Some(image.texture.clone());
                }
            }
            let created = renderer
                .gpu
                .upload_creations
                .textures
                .load(Ordering::Relaxed)
                - start;
            assert_eq!(
                created,
                (if index == 4 {
                    usize::from(version.min(2)) + 1
                } else {
                    1
                }) + usize::from(version >= 8)
            );
            if version == 7 {
                eprintln!(
                    "retired {format:?}: 8 versions, {created} texture allocations (former 8), source/GPU pins retained"
                );
            }
            if version == 2 {
                reader = None;
                cpu_lease = None;
            }
            renderer.submit(prepared).unwrap();
            registry.retire(handle);
            renderer.retire(&[resource]);
            drop(frame);
            drop(registry);
            renderer
                .gpu
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            renderer.poll().unwrap();
        }
        drop(identity);
        drop(reader);
        drop(cpu_lease);
        assert_eq!(pool.usage().bytes, 0);
        eprintln!(
            "retired {format:?}: one extra footprint adds exactly one allocation; original footprint reuses again; total={}",
            renderer
                .gpu
                .upload_creations
                .textures
                .load(Ordering::Relaxed)
                - start
        );
    }
    renderer.images.clear_idle();
    assert_eq!(renderer.budgets.resources.usage().bytes, 0);
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
#[ignore = "actual adapter mip submission; no pixel readback"]
fn mip_chains_submit_every_level_with_exact_residency() {
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 128,
            max_bytes: 8 << 20,
        },
    );
    let mut registry = ResourceRegistry::new(4, 65536).unwrap();
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        GpuLimits::default(),
    ))
    .unwrap();
    let mut renderer = Renderer::new(gpu);
    for (i, (size, format)) in [
        ([13u32, 7], TextureFormat::Rgba8),
        ([16, 8], TextureFormat::Bc3Rgba),
        ([8, 16], TextureFormat::Bc1Rgb),
        ([32, 8], TextureFormat::Bc3Rgba),
    ]
    .into_iter()
    .enumerate()
    {
        let descriptor = TextureDescriptor {
            storage_layout: if i == 3 {
                TextureStorageLayout::GxmBcMorton
            } else {
                TextureStorageLayout::Linear
            },
            mip_count: 32 - size.into_iter().max().unwrap().leading_zeros(),
            format,
            layout: TextureLayout::identity(size),
            source_encoding: ColorEncoding::Linear,
        };
        let bytes = descriptor.byte_len().unwrap();
        let handle = registry
            .register_texture(
                ResourceVersion {
                    key: AssetKey(format!("mips-{i}")),
                    version: ContentVersion([i as u8; 32]),
                },
                TexturePayload::new(
                    descriptor,
                    OwnedBytes::new(vec![128; bytes as usize], pool.reserve(bytes).unwrap())
                        .unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
        let frame = frame(registry.acquire(handle).unwrap(), 0, &pool);
        let mut attempts = 0;
        let prepared = loop {
            attempts += 1;
            assert!(attempts <= descriptor.mip_count + 3);
            match renderer.prepare(&frame) {
                Ok(p) => break p,
                Err(e) if e.is_retryable() => {
                    renderer
                        .gpu
                        .device
                        .poll(wgpu::PollType::wait_indefinitely())
                        .unwrap();
                    renderer.poll().unwrap();
                }
                Err(e) => panic!("{e:?}"),
            }
        };
        let image = &prepared.images[&handle];
        assert_eq!(image.texture.mip_level_count(), descriptor.mip_count);
        assert_eq!(image.residency.as_ref().unwrap().bytes(), bytes);
        assert_eq!(image.layout, descriptor.layout);
        eprintln!(
            "mip-submit {format:?} {size:?}: levels={} resident={bytes} prepare_attempts={attempts}",
            descriptor.mip_count
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

#[test]
#[ignore = "requires graphics adapter; compares synthetic numeric textures"]
fn atlas_views_match_standalone_crop_repeat_slices_alpha_and_mask() {
    use reel_core::texture::ImageView;
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 128,
            max_bytes: 8 << 20,
        },
    );
    let mut registry = ResourceRegistry::new(4, 1 << 20).unwrap();
    let version = |name: &str| ResourceVersion {
        key: AssetKey(name.into()),
        version: ContentVersion([1; 32]),
    };
    let desc = |size| TextureDescriptor {
        storage_layout: TextureStorageLayout::Linear,
        mip_count: 1,
        format: TextureFormat::Rgba8,
        source_encoding: ColorEncoding::Linear,
        layout: TextureLayout::identity(size),
    };
    let mut single = Vec::new();
    for y in 0..4 {
        for x in 0..4 {
            single.extend([x * 60, y * 60, 255 - x * 50, 255]);
        }
    }
    let mut page = vec![0; 12 * 12 * 4];
    for pixel in page.as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&[0, 255, 0, 255]);
    }
    for y in 0..4 {
        for x in 0..4 {
            let from = (y * 4 + x) * 4;
            let to = ((y + 4) * 12 + x + 4) * 4;
            page[to..to + 4].copy_from_slice(&single[from..from + 4]);
        }
    }
    let make = |registry: &mut ResourceRegistry, name: &str, size, bytes: Vec<u8>| {
        let n = bytes.len() as u64;
        registry
            .register_texture(
                version(name),
                TexturePayload::new(
                    desc(size),
                    OwnedBytes::new(bytes, pool.reserve(n).unwrap()).unwrap(),
                )
                .unwrap(),
            )
            .unwrap()
    };
    let single_handle = make(&mut registry, "single", [4, 4], single);
    let page_handle = make(&mut registry, "page", [12, 12], page);
    let backing = registry.acquire(page_handle).unwrap();
    let alias_handle = registry
        .register_atlas_view(
            version("alias"),
            ImageView {
                logical_size: [4, 4],
                content_origin: [4, 4],
                content_size: [4, 4],
            },
            ColorEncoding::Linear,
            backing.clone(),
        )
        .unwrap();
    let green_handle = registry
        .register_atlas_view(
            version("green"),
            ImageView {
                logical_size: [4, 4],
                content_origin: [0, 0],
                content_size: [4, 4],
            },
            ColorEncoding::Linear,
            backing,
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
    for variant in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
        let a = frame(registry.acquire(single_handle).unwrap(), variant, &pool);
        let b = frame(registry.acquire(alias_handle).unwrap(), variant, &pool);
        let ra = renderer.readback(renderer.prepare(&a).unwrap()).unwrap();
        let rb = renderer.readback(renderer.prepare(&b).unwrap()).unwrap();
        if matches!(variant, 5 | 10) {
            assert!(
                ra.iter().any(|pixel| pixel[0..3].iter().any(|v| *v > 0.05)),
                "variant {variant} produced no sampled color"
            );
        }
        let max = ra
            .iter()
            .zip(&rb)
            .flat_map(|(a, b)| a.iter().zip(b).map(|(x, y)| (x - y).abs()))
            .fold(0f32, f32::max);
        assert!(max < 0.012, "variant {variant}: max difference {max}");
    }
    // Fresh logical wrappers on the same physical page must retain the R5
    // effect binding; per-draw origin metadata remains independently updated.
    use std::sync::atomic::Ordering;
    let effects = [alias_handle, green_handle]
        .map(|handle| frame(registry.acquire(handle).unwrap(), 10, &pool));
    for (iteration, effect) in effects.iter().enumerate() {
        renderer.effect_results.clear();
        let before = renderer
            .gpu
            .upload_creations
            .effect_bind_groups
            .load(Ordering::Relaxed);
        let prepared = renderer.prepare(effect).unwrap();
        let after = renderer
            .gpu
            .upload_creations
            .effect_bind_groups
            .load(Ordering::Relaxed);
        if iteration == 1 {
            assert_eq!(
                after, before,
                "the same physical page reuses its bind group"
            );
        }
        let pixels = renderer.readback(prepared).unwrap();
        if iteration == 1 {
            let pixel = pixels[24 * 64 + 32];
            assert!(
                pixel[1] > 0.9 && pixel[0] < 0.1 && pixel[2] < 0.1,
                "{pixel:?}"
            );
        }
    }
}

#[test]
#[ignore = "requires graphics adapter; compares synthetic numeric textures"]
fn odd_atlas_views_match_standalone_sampling() {
    use reel_core::texture::ImageView;
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 128,
            max_bytes: 8 << 20,
        },
    );
    let mut registry = ResourceRegistry::new(3, 1 << 20).unwrap();
    let version = |name: &str| ResourceVersion {
        key: AssetKey(name.into()),
        version: ContentVersion([1; 32]),
    };
    let desc = |size| TextureDescriptor {
        storage_layout: TextureStorageLayout::Linear,
        mip_count: 1,
        format: TextureFormat::Rgba8,
        source_encoding: ColorEncoding::Linear,
        layout: TextureLayout::identity(size),
    };
    let mut single = Vec::with_capacity(5 * 7 * 4);
    for y in 0..7 {
        for x in 0..5 {
            single.extend([x * 40, y * 30, 255 - x * 40, 255]);
        }
    }
    let mut page = vec![0; 16 * 16 * 4];
    for pixel in page.as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&[0, 255, 0, 255]);
    }
    for y in 0..7 {
        for x in 0..5 {
            let from = (y * 5 + x) * 4;
            let to = ((y + 4) * 16 + x + 4) * 4;
            page[to..to + 4].copy_from_slice(&single[from..from + 4]);
        }
    }
    let make = |registry: &mut ResourceRegistry, name: &str, size, bytes: Vec<u8>| {
        let n = bytes.len() as u64;
        registry
            .register_texture(
                version(name),
                TexturePayload::new(
                    desc(size),
                    OwnedBytes::new(bytes, pool.reserve(n).unwrap()).unwrap(),
                )
                .unwrap(),
            )
            .unwrap()
    };
    let single_handle = make(&mut registry, "single", [5, 7], single);
    let page_handle = make(&mut registry, "page", [16, 16], page);
    let backing = registry.acquire(page_handle).unwrap();
    let alias_handle = registry
        .register_atlas_view(
            version("alias"),
            ImageView {
                logical_size: [5, 7],
                content_origin: [4, 4],
                content_size: [5, 7],
            },
            ColorEncoding::Linear,
            backing,
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
    for variant in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
        let a = frame(registry.acquire(single_handle).unwrap(), variant, &pool);
        let b = frame(registry.acquire(alias_handle).unwrap(), variant, &pool);
        let ra = renderer.readback(renderer.prepare(&a).unwrap()).unwrap();
        let rb = renderer.readback(renderer.prepare(&b).unwrap()).unwrap();
        if matches!(variant, 5 | 10) {
            assert!(
                ra.iter().any(|pixel| pixel[0..3].iter().any(|v| *v > 0.05)),
                "variant {variant} produced no sampled color"
            );
        }
        let max = ra
            .iter()
            .zip(&rb)
            .flat_map(|(a, b)| a.iter().zip(b).map(|(x, y)| (x - y).abs()))
            .fold(0f32, f32::max);
        assert!(max < 0.012, "variant {variant}: max difference {max}");
    }
    // Fresh logical wrappers on the same physical page must retain the R5
    // effect binding; per-draw origin metadata remains independently updated.
    use std::sync::atomic::Ordering;
    let effect = frame(registry.acquire(alias_handle).unwrap(), 10, &pool);
    for iteration in 0..2 {
        renderer.effect_results.clear();
        let before = renderer
            .gpu
            .upload_creations
            .effect_bind_groups
            .load(Ordering::Relaxed);
        let prepared = renderer.prepare(&effect).unwrap();
        let after = renderer
            .gpu
            .upload_creations
            .effect_bind_groups
            .load(Ordering::Relaxed);
        if iteration == 1 {
            assert_eq!(
                after, before,
                "the same physical page reuses its bind group"
            );
        }
        renderer.submit(prepared).unwrap();
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
    }
}

fn effect_frame(lease: ResourceLease, pool: &BudgetPool) -> RenderFrame {
    use reel_ui::{
        core::PaintEffect,
        imaging::{
            Composite,
            record::{Draw, Geometry, Scene},
        },
        kurbo::Affine,
        peniko::{Brush, Color},
        resources::{EffectShaderDescriptor, EffectShaderFormat},
        stage::effect::*,
    };
    let program = EffectProgram::with_textures(
        vec![],
        vec![EffectTextureParameter { name: "ink".into() }],
        vec![
            EffectNode {
                ty: EffectType::Vec2,
                op: EffectOp::Input(EffectInput::Uv),
            },
            EffectNode {
                ty: EffectType::Vec4,
                op: EffectOp::TextureSample {
                    binding: 0,
                    uv: EffectNodeId(0),
                    channel: EffectTextureChannel::Color,
                },
            },
        ],
        EffectNodeId(1),
    )
    .unwrap();
    let fragment =
        crate::effect::compile_fragment(&program, crate::effect::Language::Wgsl, 16384).unwrap();
    let desc = EffectShaderDescriptor::with_textures(
        [9; 32],
        EffectShaderFormat::Wgsl,
        vec![],
        1,
        fragment.source_sampler,
        EffectType::Vec4,
        fragment.source.as_bytes(),
    )
    .unwrap();
    let mut registry = ResourceRegistry::new(1, 1 << 20).unwrap();
    let data = fragment.source.as_bytes().to_vec();
    let bytes = data.len() as u64;
    let shader = registry
        .register_effect_shader(
            ResourceVersion {
                key: AssetKey("effect".into()),
                version: ContentVersion(desc.shader_key),
            },
            desc,
            OwnedBytes::new(data, pool.reserve(bytes).unwrap()).unwrap(),
        )
        .unwrap();
    let shader = registry.acquire(shader).unwrap();
    let bounds = Rect::new(0., 0., 64., 48.);
    let mut scene = Scene::new();
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::WHITE),
        brush_transform: None,
        shape: Geometry::Rect(bounds),
        composite: Composite::default(),
    });
    let mut widget = r01_fixture::RecordWidget::new(scene);
    widget.clip = false;
    widget.effect = Some(PaintEffect {
        instance: EffectInstance::with_textures([9; 32], program, vec![], vec!["ink".into()])
            .unwrap(),
        shader: shader.clone(),
        images: vec![lease.clone()],
        bounds,
        coverage_invert: false,
    });
    let mut bridge = r01_fixture::widget_bridge(widget, Affine::IDENTITY);
    let resources = ResourceSnapshot::capture([Ok(lease), Ok(shader)].into_iter(), pool).unwrap();
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        resources,
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
