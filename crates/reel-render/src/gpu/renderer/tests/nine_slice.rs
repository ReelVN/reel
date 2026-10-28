//! Image widget -> portable frame -> GPU, including saved retained pictures.
use super::*;
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, ResourceVersion},
};
use reel_ui::{
    core::NewWidget,
    frame::{FrameDraw, FrameOp, FrameTransition, RetainBuild, SavedRetainedScene},
    kurbo::{Insets, Point, Size},
    peniko::{ImageAlphaType, ImageBrush, ImageFormat},
    properties::ObjectFit,
    resources::{ColorEncoding, ResourceRegistry, ResourceSnapshot},
    widgets::{Image, ImageTile},
};

fn packed_alpha_frame(
    pool: &BudgetPool,
    video: bool,
    transfer: reel_core::image::YuvTransfer,
    layout: reel_ui::frame::ImageAlphaLayout,
    crop: Option<Rect>,
) -> RenderFrame {
    use reel_core::image::{Yuv420Image, Yuv420Storage, YuvColor, YuvMatrix};
    use std::sync::Arc;
    #[derive(Debug)]
    struct Planes([Vec<u8>; 3]);
    impl Yuv420Storage for Planes {
        fn plane(&self, index: usize) -> &[u8] {
            &self.0[index]
        }
    }
    let mut registry = ResourceRegistry::new(1, 64 << 10).unwrap();
    let version = ResourceVersion {
        key: AssetKey("packed-alpha".into()),
        version: ContentVersion([3; 32]),
    };
    let handle = if video {
        let full_range = transfer == reel_core::image::YuvTransfer::Srgb;
        let mut y = vec![if full_range { 255 } else { 235 }; 64];
        y[32..].fill(if full_range { 128 } else { 126 });
        registry
            .register_yuv420(
                version,
                Yuv420Image::new(
                    [8, 8],
                    YuvColor {
                        matrix: YuvMatrix::Bt709,
                        full_range,
                        transfer,
                    },
                    Arc::new(Planes([y, vec![128; 16], vec![128; 16]])),
                )
                .unwrap(),
            )
            .unwrap()
    } else {
        let pixels = (0..64)
            .flat_map(|i| {
                if i < 32 {
                    [255, 255, 255, 64]
                } else {
                    [128, 0, 255, 255]
                }
            })
            .collect::<Vec<_>>();
        registry
            .register_image(
                version,
                OwnedBytes::new(pixels, pool.reserve(256).unwrap()).unwrap(),
                [8, 8],
                ImageFormat::Rgba8,
                ImageAlphaType::Alpha,
                if transfer == reel_core::image::YuvTransfer::Srgb {
                    ColorEncoding::Srgb
                } else {
                    ColorEncoding::Linear
                },
            )
            .unwrap()
    };
    let mut widget = RecordWidget::new(Scene::new());
    widget.inner.add(
        NewWidget::new(
            Image::from_resource(registry.acquire(handle).unwrap())
                .with_alpha_layout(layout)
                .with_source_rect(crop),
        )
        .with_props(ObjectFit::Contain),
        Point::ZERO,
        Size::new(200., 200.),
    );
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    build(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[handle], pool).unwrap(),
        pool,
    )
}

#[test]
fn packed_alpha_natural_size_crop_and_retained_appearance_share_one_source() {
    use reel_ui::frame::ImageAlphaLayout;
    let pool = pool();
    assert_eq!(
        ImageAlphaLayout::TopBottom.size(Size::new(960., 1024.)),
        Size::new(960., 512.)
    );
    for crop in [None, Some(Rect::new(2., 1., 6., 3.))] {
        let frame = packed_alpha_frame(
            &pool,
            true,
            reel_core::image::YuvTransfer::Bt709,
            ImageAlphaLayout::TopBottom,
            crop,
        );
        assert_eq!(frame.resources().leases().len(), 1);
        let image = frame
            .commands()
            .iter()
            .find_map(|command| match &command.op {
                FrameOp::Draw(FrameDraw::Fill {
                    brush: Brush::Image(brush),
                    transform,
                    shape: Geometry::Rect(bounds),
                    ..
                }) => Some((brush.image, *transform, *bounds)),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            image.0.source_rect,
            crop.or(Some(Rect::new(0., 0., 8., 4.)))
        );
        assert_eq!(
            image.1.transform_rect_bbox(image.2),
            Rect::new(0., 50., 200., 150.)
        );
        assert_eq!(
            image.0.appearance.unwrap().alpha_layout,
            ImageAlphaLayout::TopBottom
        );
        let bytes = postcard::to_allocvec(&image.0).unwrap();
        assert_eq!(
            postcard::from_bytes::<reel_ui::frame::FrameImage>(&bytes).unwrap(),
            image.0
        );
    }
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn packed_alpha_uses_encoded_half_coverage_and_preserves_native_pixels() {
    use reel_core::image::YuvTransfer;
    use reel_ui::frame::ImageAlphaLayout;
    let pool = pool();
    let mut renderer = renderer(Default::default());
    eprintln!("packed alpha adapter={:?}", renderer.gpu.adapter.get_info());
    for video in [false, true] {
        for transfer in [YuvTransfer::Srgb, YuvTransfer::Bt709] {
            for crop in [None, Some(Rect::new(2., 1., 6., 3.))] {
                let frame =
                    packed_alpha_frame(&pool, video, transfer, ImageAlphaLayout::TopBottom, crop);
                let textures = renderer
                    .gpu
                    .upload_creations
                    .textures
                    .load(std::sync::atomic::Ordering::Relaxed);
                let pixels = renderer
                    .readback(renderer.prepare(&frame).unwrap())
                    .unwrap();
                let created = renderer
                    .gpu
                    .upload_creations
                    .textures
                    .load(std::sync::atomic::Ordering::Relaxed)
                    - textures;
                assert!(created <= if video { 3 } else { 1 });
                for y in [50, 75, 125, 149] {
                    let pixel = pixels[y * 400 + 100];
                    assert!(
                        pixel.iter().all(|v| (*v - 128. / 255.).abs() < 0.006),
                        "video={video}, transfer={transfer:?}, crop={crop:?}: {pixel:?}"
                    );
                }
                assert_eq!(pixels[25 * 400 + 100], [0.; 4]);
                assert_eq!(pixels[175 * 400 + 100], [0.; 4]);
            }
            let frame = packed_alpha_frame(&pool, video, transfer, ImageAlphaLayout::Native, None);
            let pixels = renderer
                .readback(renderer.prepare(&frame).unwrap())
                .unwrap();
            let expected = if video { 1. } else { 64. / 255. };
            assert!(
                pixels[25 * 400 + 100]
                    .iter()
                    .all(|v| (*v - expected).abs() < 0.006)
            );
        }
    }
}

fn sliced_frame(pool: &BudgetPool) -> RenderFrame {
    // Exact 20x73 source with a split at x=10. Bilinear sampling of that
    // boundary must remain half red/half blue across a collapsed center.
    let mut rgba = Vec::with_capacity(20 * 73 * 4);
    for y in 0..73 {
        for x in 0..20 {
            rgba.extend([
                if x < 10 { 255 } else { 0 },
                if y < 36 { 0 } else { 255 },
                if x < 10 { 0 } else { 255 },
                255,
            ]);
        }
    }
    let mut registry = ResourceRegistry::new(1, 64 << 10).unwrap();
    let handle = registry
        .register_image(
            ResourceVersion {
                key: AssetKey("zero-span-panel".into()),
                version: ContentVersion([1; 32]),
            },
            OwnedBytes::new(rgba, pool.reserve(20 * 73 * 4).unwrap()).unwrap(),
            [20, 73],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    let lease = registry.acquire(handle).unwrap();
    let image = crate::test_support::image_data(&lease);
    let mut background = Scene::new();
    fill(&mut background, Color::BLACK);
    // A path image takes the generic mesh route rather than rectangle batching.
    background.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Image(ImageBrush::new(image.clone()).with_alpha(0.5)),
        brush_transform: Some(
            Affine::translate((350., 210.)) * Affine::scale_non_uniform(2., 80. / 73.),
        ),
        shape: Geometry::Path(Rect::new(350., 210., 390., 290.).to_path(0.1)),
        composite: Composite::default(),
    });
    let mut widget = RecordWidget::new(background);
    let horizontal = Insets::new(10., 0., 10., 0.);
    let vertical = Insets::new(0., 36.5, 0., 36.5);
    let both = Insets::new(10., 36.5, 10., 36.5);
    for (origin, size, slice, center) in [
        ((10., 10.), (150., 80.), Some(horizontal), true),
        ((200., 10.), (150., 80.), Some(vertical), true),
        ((10., 110.), (150., 80.), Some(both), true),
        ((180., 110.), (12., 30.), Some(both), true),
        ((200., 110.), (150., 80.), Some(both), false),
        ((10., 210.), (150., 80.), None, true),
    ] {
        widget.inner.add(
            NewWidget::new(Image::from_resource(lease.clone()).with_slice(slice, 1., center))
                .with_props(ObjectFit::Stretch),
            Point::from(origin),
            Size::from(size),
        );
    }
    widget.inner.add(
        NewWidget::new(Image::tiled(
            Size::new(20., 73.),
            vec![ImageTile {
                resource: lease.clone(),
                transform: Affine::IDENTITY,
                clip: Rect::new(0., 0., 20., 73.),
            }],
        ))
        .with_props(ObjectFit::Stretch),
        Point::new(200., 210.),
        Size::new(150., 80.),
    );
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    build(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[handle], pool).unwrap(),
        pool,
    )
}

fn assert_samples(pixels: &[[f32; 4]]) {
    let check = |x: usize, y: usize, expected: [f32; 4]| {
        let actual = pixels[y * 400 + x];
        assert!(
            actual
                .iter()
                .zip(expected)
                .all(|(a, b)| (*a - b).abs() < 0.012),
            "({x},{y}): {actual:?}, expected {expected:?}"
        );
    };
    // Horizontal collapsed center, far apart; source left/right corners retain color.
    check(40, 30, [0.5, 0., 0.5, 1.]);
    check(130, 30, [0.5, 0., 0.5, 1.]);
    check(13, 30, [1., 0., 0., 1.]);
    check(156, 30, [0., 0., 1., 1.]);
    // Vertical and both-axis collapsed centers; omitted center stays background.
    check(220, 50, [1., 1., 0., 1.]);
    check(330, 50, [0., 1., 1., 1.]);
    check(40, 150, [0.5, 1., 0.5, 1.]);
    check(130, 150, [0.5, 1., 0.5, 1.]);
    check(275, 150, [0., 0., 0., 1.]);
    // A too-small target still fits the two borders, with no hole or overlap.
    check(181, 112, [1., 0., 0., 1.]);
    check(190, 137, [0., 1., 1., 1.]);
    // Ordinary images keep the original mapping direction after FrameBuild conversion.
    check(30, 230, [1., 0., 0., 1.]);
    check(140, 270, [0., 1., 1., 1.]);
    // Tile transforms and the generic image path share the same source mapping.
    check(220, 230, [1., 0., 0., 1.]);
    check(330, 270, [0., 1., 1., 1.]);
    check(355, 230, [0.5, 0., 0., 1.]);
    check(385, 270, [0., 0.5, 0.5, 1.]);
}

#[test]
fn singular_painter_image_transform_is_rejected_at_frame_conversion() {
    let pool = pool();
    let mut registry = ResourceRegistry::new(1, 64 << 10).unwrap();
    let handle = registry
        .register_image(
            ResourceVersion {
                key: AssetKey("singular-image".into()),
                version: ContentVersion([2; 32]),
            },
            OwnedBytes::new(vec![255; 4], pool.reserve(4).unwrap()).unwrap(),
            [1, 1],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    let lease = registry.acquire(handle).unwrap();
    let image = crate::test_support::image_data(&lease);
    let mut scene = Scene::new();
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Image(ImageBrush::new(image.clone())),
        brush_transform: Some(Affine::scale_non_uniform(0., 2.)),
        shape: Geometry::Rect(Rect::new(0., 0., 100., 100.)),
        composite: Composite::default(),
    });
    let mut bridge = widget_bridge(RecordWidget::new(scene), Affine::IDENTITY);
    let mut frame = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[handle], &pool).unwrap(),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        limits(),
        &pool,
    )
    .unwrap();
    loop {
        match frame.step(step_budget()) {
            Err(error) => {
                assert_eq!(error.kind, reel_ui::frame::BuildErrorKind::InvalidRecord);
                break;
            }
            Ok(progress) => assert!(!progress.complete, "invalid inverse reached a render frame"),
        }
    }
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn zero_span_nine_slice_samples_lines_points_and_restored_pictures() {
    let pool = pool();
    let frame = sliced_frame(&pool);
    let mut renderer = renderer(Default::default());
    let pixels = renderer
        .readback(renderer.prepare(&frame).unwrap())
        .unwrap();
    assert_samples(&pixels);
    counts(&renderer, 2); // One shared image and one shared solid-color texture.

    let mut retain = RetainBuild::new(&frame, None, limits(), &pool).unwrap();
    let mut budget = step_budget();
    loop {
        let p = retain.step(budget).unwrap();
        if p.complete {
            break;
        }
        budget.max_units = budget.max_units.max(p.minimum_units);
        budget.max_bytes = budget.max_bytes.max(p.minimum_bytes);
    }
    let retained = retain.finish().unwrap();
    let saved = retained.snapshot(|_| None).unwrap();
    let bytes = postcard::to_allocvec(&saved).unwrap();
    let saved: SavedRetainedScene = postcard::from_bytes(&bytes).unwrap();
    let mut restore = saved.restore_begin(limits(), &pool).unwrap();
    loop {
        let p = restore.step(budget).unwrap();
        if p.build.complete {
            break;
        }
        budget.max_units = budget.max_units.max(p.build.minimum_units);
        budget.max_bytes = budget.max_bytes.max(p.build.minimum_bytes);
    }
    let restored = restore.finish().unwrap();
    let mut bridge = widget_bridge(RecordWidget::new(Scene::new()), Affine::IDENTITY);
    let mut next = FrameBuild::new(
        bridge.record(FrameId(2)).unwrap(),
        empty_snapshot(&pool),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        limits(),
        &pool,
    )
    .unwrap();
    next.set_transitions(vec![FrameTransition {
        root: None,
        previous: restored,
        progress: 0.,
        mask: None,
    }])
    .unwrap();
    while !next.step(step_budget()).unwrap().complete {}
    let next = next.finish().unwrap();
    let restored_pixels = renderer.readback(renderer.prepare(&next).unwrap()).unwrap();
    assert_samples(&restored_pixels);
    assert_eq!(
        pixels, restored_pixels,
        "serialized retained mapping changed pixels"
    );
}

#[test]
fn image_windows_repeat_and_zero_span_slices_keep_shared_frame_maps_without_device() {
    let pool = pool();
    let sliced = sliced_frame(&pool);
    let source_maps = sliced
        .commands()
        .iter()
        .filter_map(|command| match &command.op {
            FrameOp::Draw(FrameDraw::Fill {
                brush: Brush::Image(_),
                brush_transform: Some(map),
                ..
            }) => Some(*map),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        source_maps
            .iter()
            .any(|map| map.as_coeffs()[0] == 0. && map.as_coeffs()[4] == 10.),
        "collapsed horizontal slice retains source x=10"
    );
    assert!(
        source_maps
            .iter()
            .any(|map| map.as_coeffs()[3] == 0. && map.as_coeffs()[5] == 36.5),
        "collapsed vertical slice retains source y=36.5"
    );
    let lease = sliced.resources().leases()[0].clone();
    let handle = lease.handle();
    let mut widget = RecordWidget::new(Scene::new());
    for (index, rect, repeat) in [
        (0, None, [true; 2]),
        (1, Some(Rect::new(10., 0., 20., 73.)), [true; 2]),
        (2, Some(Rect::new(0., 0., 10., 73.)), [false; 2]),
    ] {
        widget.inner.add(
            NewWidget::new(
                Image::from_resource(lease.clone())
                    .with_source_rect(rect)
                    .with_pattern(repeat, 2., None),
            )
            .with_props(ObjectFit::Stretch),
            Point::new(f64::from(index) * 100., 0.),
            Size::new(20., 146.),
        );
    }
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        ResourceSnapshot::capture([Ok(lease)].into_iter(), &pool).unwrap(),
        &pool,
    );
    let images = frame
        .commands()
        .iter()
        .enumerate()
        .filter_map(|(index, command)| match &command.op {
            FrameOp::Draw(FrameDraw::Fill {
                brush: Brush::Image(image),
                brush_transform: Some(map),
                ..
            }) => Some((index, image, *map)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(images.len(), 3);
    assert_eq!(frame.resources().leases().len(), 1);
    assert!(
        images
            .iter()
            .all(|(_, image, _)| image.image.resource == handle)
    );
    assert_eq!(
        images[0].2,
        Affine::scale(0.5),
        "ordinary repeat retains its original transform convention"
    );
    assert_eq!(
        images[1].2,
        Affine::translate((10., 0.)) * Affine::scale(0.5)
    );
    assert_eq!(images[2].2, Affine::IDENTITY);
    assert_eq!(
        images[2].0,
        images[1].0 + 1,
        "image regions add no clip/material boundary between shared-texture quads"
    );
    assert_eq!(
        images[1].1.image.source_rect,
        Some(Rect::new(10., 0., 20., 73.))
    );
}
