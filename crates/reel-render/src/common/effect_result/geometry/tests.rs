use super::*;
use crate::{common::renderer::Budgets, test_support::r01_fixture};
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
    texture::{
        ColorEncoding, ImageView, TextureDescriptor, TextureFormat, TexturePayload,
        TextureStorageLayout,
    },
};
use reel_ui::{
    frame::{FrameOp, RecordLocation, RenderFrame},
    imaging::record::Scene,
    peniko::{ImageAlphaType, ImageFormat},
    resources::{ResourceLease, ResourceRegistry},
};

fn version(name: &str) -> ResourceVersion {
    ResourceVersion {
        key: AssetKey(name.into()),
        version: ContentVersion([7; 32]),
    }
}

fn portrait() -> (ResourceRegistry, ResourceLease) {
    let size = [64, 192];
    let mut pixels = vec![0; size[0] as usize * size[1] as usize * 4];
    for y in 3..100 {
        pixels[((y * size[0] + 32) * 4 + 3) as usize] = 255;
    }
    for y in 150..190 {
        for x in 16..48 {
            pixels[((y * size[0] + x) * 4 + 3) as usize] = 128;
        }
    }
    let pool = r01_fixture::pool();
    let bytes = pixels.len() as u64;
    let mut registry = ResourceRegistry::new(4, 1024 * 1024).unwrap();
    let handle = registry
        .register_image(
            version("raster-portrait"),
            OwnedBytes::new(pixels, pool.reserve(bytes).unwrap()).unwrap(),
            size,
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Linear,
        )
        .unwrap();
    let lease = registry.acquire(handle).unwrap();
    (registry, lease)
}

fn frame(registry: &ResourceRegistry, source: &ResourceLease) -> RenderFrame {
    let pool = r01_fixture::pool();
    let size = source.image_size().unwrap();
    let mut widget = r01_fixture::RecordWidget::new(Scene::new());
    widget.clip = false;
    widget.resource_draws.push((
        source.clone(),
        Rect::new(0., 0., f64::from(size[0]), f64::from(size[1])),
        Affine::IDENTITY,
        ImageSampler::default(),
    ));
    let mut bridge = r01_fixture::widget_bridge(widget, Affine::IDENTITY);
    r01_fixture::build(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[source.handle()], &pool).unwrap(),
        &pool,
    )
}

fn input(frame: &RenderFrame) -> (&FrameDraw, RecordLocation) {
    frame
        .commands()
        .iter()
        .find_map(|command| match &command.op {
            FrameOp::Draw(
                draw @ FrameDraw::Fill {
                    brush: Brush::Image(_),
                    ..
                },
            ) => Some((draw, command.origin.at)),
            _ => None,
        })
        .unwrap()
}

fn payload(key: &Key, at: RecordLocation) -> (Vec<[u32; 8]>, Vec<u32>) {
    let limits = crate::common::GpuLimits::default();
    let shared = Budgets::new(limits);
    let local = Budgets::new(limits);
    let mut mesh = mesh::Mesh::new(mesh::Allocation::new(
        &shared,
        &local,
        limits.transient_bytes,
        1024 * 1024,
    ));
    for rect in key.quads().unwrap().iter() {
        mesh.image_quad(
            rect,
            key.physical,
            key.uv(),
            key.size,
            mesh::ImagePaint {
                color: [1.; 4],
                sampling: mesh::ImageSampling::for_image(
                    key.source_rect,
                    key.layout,
                    key.normalized_origin(),
                    key.repeat(),
                    key.atlas,
                ),
            },
            at,
        )
        .unwrap();
    }
    let vertices = mesh
        .vertices
        .iter()
        .map(|v| {
            [
                v.position[0],
                v.position[1],
                v.uv[0],
                v.uv[1],
                v.source_window[0],
                v.source_window[1],
                v.source_window[2],
                v.source_window[3],
            ]
            .map(f32::to_bits)
        })
        .collect();
    (vertices, mesh.indices)
}

#[test]
fn alpha_coverage_batches_compare_the_actual_vertices_without_copying_them_into_keys() {
    let (registry, source) = portrait();
    let frame = frame(&registry, &source);
    let (draw, at) = input(&frame);
    let make = |transform| image(frame.drawing(), draw, transform, [512; 2]).unwrap();
    let base = make(Affine::translate((224., 32.)));
    assert!(base.quads().unwrap().iter().count() > 1);
    assert!(size_of::<Key>() < 320);
    let same_pixels = make(Affine::translate((224. + 2f64.powi(-40), 32.)));
    assert_ne!(
        super::super::affine(base.physical),
        super::super::affine(same_pixels.physical)
    );
    assert_eq!(payload(&base, at), payload(&same_pixels, at));
    assert!(base == same_pixels);

    for transform in [
        Affine::translate((225., 32.)),
        Affine::translate((160., 40.)) * Affine::rotate(0.37),
        Affine::translate((220., 220.)) * Affine::scale_non_uniform(-0.7, 0.6),
    ] {
        let changed = make(transform);
        assert_eq!(base == changed, payload(&base, at) == payload(&changed, at));
    }
}

#[test]
fn equal_original_corners_do_not_hide_changed_internal_coverage_vertices() {
    let (registry, source) = portrait();
    let frame = frame(&registry, &source);
    let (draw, at) = input(&frame);
    let base = image(
        frame.drawing(),
        draw,
        Affine::translate((224., 32.)),
        [512; 2],
    )
    .unwrap();
    let shifted = image(
        frame.drawing(),
        draw,
        Affine::translate((224. + 2f64.powi(-22), 32.)),
        [512; 2],
    )
    .unwrap();
    let corners = |key: &Key| {
        mesh::quad_local(key.bounds)
            .map(|p| mesh::project_position(p, key.physical, key.size).map(f32::to_bits))
    };
    assert_eq!(corners(&base), corners(&shifted));
    assert_ne!(payload(&base, at), payload(&shifted, at));
    assert!(base != shifted);
}

#[test]
fn prepared_storage_compaction_and_atlas_windows_follow_the_shared_upload_layout() {
    let pool = r01_fixture::pool();
    let descriptor = TextureDescriptor {
        storage_layout: TextureStorageLayout::Linear,
        mip_count: 1,
        format: TextureFormat::Rgba8,
        layout: TextureLayout {
            logical_size: [40, 50],
            content_size: [40, 50],
            storage_size: [128, 128],
        },
        source_encoding: ColorEncoding::Linear,
    };
    let bytes = descriptor.byte_len().unwrap();
    let pixels = OwnedBytes::new(vec![255; bytes as usize], pool.reserve(bytes).unwrap()).unwrap();
    let mut registry = ResourceRegistry::new(4, 1024 * 1024).unwrap();
    let backing = registry
        .register_texture(
            version("raster-page"),
            TexturePayload::new(descriptor, pixels).unwrap(),
        )
        .unwrap();
    let backing = registry.acquire(backing).unwrap();
    let view = registry
        .register_atlas_view(
            version("raster-view"),
            ImageView {
                logical_size: [32, 48],
                content_origin: [8, 8],
                content_size: [16, 24],
            },
            ColorEncoding::Linear,
            backing.clone(),
        )
        .unwrap();
    let view = registry.acquire(view).unwrap();
    for source in [&backing, &view] {
        let frame = frame(&registry, source);
        let (draw, at) = input(&frame);
        let key = image(frame.drawing(), draw, Affine::IDENTITY, [512; 2]).unwrap();
        assert_eq!(key.layout.storage_size, [64, 64]);
        assert_eq!(key.atlas, source.handle() == view.handle());
        let window = key.window().map(f32::from_bits);
        if key.atlas {
            assert!(!key.coverage);
            assert_eq!(window, [0.125, 0.125, 0.25, 0.375]);
        } else {
            assert_eq!(window, [0., 0., 0.625, 0.78125]);
        }
        let mut changed = key.clone();
        changed.brush = Affine::translate((0.5, -0.25));
        assert_ne!(payload(&key, at), payload(&changed, at));
        assert!(key != changed);
    }
}

#[test]
fn unsupported_sampler_and_invisible_rectangles_keep_the_affine_fallback() {
    let (registry, source) = portrait();
    let frame = frame(&registry, &source);
    let (draw, _) = input(&frame);
    assert!(
        image(
            frame.drawing(),
            draw,
            Affine::translate((-2000., 0.)),
            [512; 2]
        )
        .is_none()
    );
    let mut changed = draw.clone();
    let FrameDraw::Fill {
        brush: Brush::Image(brush),
        ..
    } = &mut changed
    else {
        unreachable!()
    };
    brush.sampler.quality = ImageQuality::High;
    assert!(image(frame.drawing(), &changed, Affine::IDENTITY, [512; 2]).is_none());
    let FrameDraw::Fill {
        brush: Brush::Image(brush),
        ..
    } = &mut changed
    else {
        unreachable!()
    };
    brush.sampler.quality = ImageQuality::Medium;
    brush.sampler.x_extend = Extend::Reflect;
    assert!(image(frame.drawing(), &changed, Affine::IDENTITY, [512; 2]).is_none());
}

#[test]
fn coordinate_observations_do_not_pin_source_pixel_storage() {
    let (registry, source) = portrait();
    let frame = frame(&registry, &source);
    let (draw, _) = input(&frame);
    let key = image(
        frame.drawing(),
        draw,
        Affine::translate((224., 32.)),
        [512; 2],
    )
    .unwrap();
    let mut changed = key.clone();
    changed.physical = Affine::translate((224. + 2f64.powi(-40), 32.));
    assert!(key == changed);
    drop(frame);
    drop(source);
    drop(registry);
    assert!(key.source.is_released());
    assert!(key.quads().is_none());
    assert!(key == key.clone());
    assert!(key != changed);
}
