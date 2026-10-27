use super::*;
use reel_ui::{
    frame::{ImageAppearance, ImageColorAdjustment},
    kurbo::Point,
    peniko::ImageSampler,
    resources::ResourceHandle,
};

fn brush(source_rect: Option<Rect>) -> ImageBrush<FrameImage> {
    ImageBrush {
        image: FrameImage {
            resource: ResourceHandle {
                registry: 1,
                slot: 0,
                generation: 1,
            },
            color: [1.; 4],
            appearance: None,
            source_rect,
        },
        sampler: ImageSampler::default(),
    }
}

fn portrait(size: [u32; 2]) -> Vec<u8> {
    let mut pixels = vec![0; size[0] as usize * size[1] as usize * 4];
    for y in 3..size[1] - 2 {
        // A narrowing head and wider shoulders exercise several bands, with
        // nonzero samples exactly at band edges and at fractional-alpha edges.
        let half = if y < size[1] / 2 {
            3 + y / 3
        } else {
            9 + y / 5
        };
        for x in size[0] / 2 - half..size[0] / 2 + half {
            pixels[((y * size[0] + x) * 4 + 3) as usize] = if x % 3 == 0 { 1 } else { 255 };
        }
    }
    pixels
}

fn sampled_alpha(pixels: &[u8], size: [u32; 2], at: Point, window: Rect, nearest: bool) -> f64 {
    let pixel = |x: i32, y: i32| {
        let x = x.clamp(0, size[0] as i32 - 1) as u32;
        let y = y.clamp(0, size[1] as i32 - 1) as u32;
        f64::from(pixels[((y * size[0] + x) * 4 + 3) as usize])
    };
    let x = at.x.clamp(
        window.x0 + 0.5_f64.min(window.width() * 0.5),
        window.x1 - 0.5_f64.min(window.width() * 0.5),
    );
    let y = at.y.clamp(
        window.y0 + 0.5_f64.min(window.height() * 0.5),
        window.y1 - 0.5_f64.min(window.height() * 0.5),
    );
    if nearest {
        return pixel(x.floor() as i32, y.floor() as i32);
    }
    let [x, y] = [x - 0.5, y - 0.5];
    let [x0, y0] = [x.floor() as i32, y.floor() as i32];
    let [tx, ty] = [x.fract(), y.fract()];
    pixel(x0, y0) * (1. - tx) * (1. - ty)
        + pixel(x0 + 1, y0) * tx * (1. - ty)
        + pixel(x0, y0 + 1) * (1. - tx) * ty
        + pixel(x0 + 1, y0 + 1) * tx * ty
}

#[test]
fn source_windows_scaling_mirroring_and_filter_neighbours_keep_every_nonzero_sample() {
    let size = [64, 65];
    let pixels = portrait(size);
    let coverage = ImageCoverage::rgba8(size, &pixels).unwrap();
    let layout = TextureLayout {
        logical_size: [192, 130],
        content_size: size,
        storage_size: [64, 128],
    };
    for crop in [
        None,
        Some(Rect::new(42., 8., 150., 121.)),
        Some(Rect::new(96., 18., 96.75, 18.75)),
    ] {
        let brush = brush(crop);
        let source = crop.unwrap_or(Rect::new(0., 0., 192., 130.));
        for mirror in [false, true] {
            let bounds = Rect::new(4., 7., 124., 87.);
            let transform =
                Affine::translate((if mirror { source.x1 } else { source.x0 }, source.y0))
                    * Affine::scale_non_uniform(
                        source.width() / bounds.width() * if mirror { -1. } else { 1. },
                        source.height() / bounds.height(),
                    )
                    * Affine::translate((-bounds.x0, -bounds.y0));
            let physical_transform = Affine::rotate(0.7) * Affine::scale(0.25);
            let quads = quads(
                bounds,
                &brush,
                transform,
                physical_transform,
                layout,
                Some(&coverage),
            );
            assert!(quads.len <= IMAGE_COVERAGE_BANDS);
            if crop.is_none() || crop.is_some_and(|rect| rect.width() > 100.) {
                assert!(quads.iter().map(|rect| rect.area()).sum::<f64>() < bounds.area());
            }
            for (i, a) in quads.iter().enumerate() {
                assert!(a.width() > 0. && a.height() > 0.);
                for b in quads.iter().skip(i + 1) {
                    let overlap = a.intersect(b);
                    assert!(overlap.width() <= 1e-12 || overlap.height() <= 1e-12);
                }
            }
            for nearest in [false, true] {
                for y in 0..320 {
                    for x in 0..480 {
                        let local = Point::new(
                            bounds.x0 + (f64::from(x) + 0.37) / 4.,
                            bounds.y0 + (f64::from(y) + 0.61) / 4.,
                        );
                        let authored = transform * local;
                        let texel = Point::new(authored.x / 3., authored.y / 2.);
                        let window = Rect::new(
                            source.x0 / 3.,
                            source.y0 / 2.,
                            source.x1 / 3.,
                            source.y1 / 2.,
                        );
                        if sampled_alpha(&pixels, size, texel, window, nearest) > 0. {
                            assert!(
                                quads.iter().any(|quad| quad.contains(local)),
                                "nonzero sample removed: {local:?}, {crop:?}, mirror={mirror}, nearest={nearest}"
                            );
                            for offset in [(0.9375, -0.9375), (-0.9375, 0.9375)] {
                                let sample = physical_transform.inverse()
                                    * (physical_transform * local
                                        + reel_ui::kurbo::Vec2::from(offset));
                                if bounds.contains(sample) {
                                    assert!(
                                        quads.iter().any(|quad| quad.contains(sample)),
                                        "coverage sample removed: {sample:?}, shade={local:?}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let full = Rect::new(0., 0., 192., 130.);
    let quads = quads(
        full,
        &brush(None),
        Affine::IDENTITY,
        Affine::IDENTITY,
        layout,
        Some(&coverage),
    );
    let area: f64 = quads.iter().map(|rect| rect.area()).sum();
    assert!(
        area < full.area() * 0.6,
        "portrait should remove substantial transparent fill: {area}"
    );
}

#[test]
fn repetition_packed_alpha_unknown_or_crossing_source_domains_keep_original_geometry() {
    let bounds = Rect::new(0., 0., 64., 65.);
    let pixels = portrait([64, 65]);
    let coverage = ImageCoverage::rgba8([64, 65], &pixels).unwrap();
    let layout = TextureLayout::identity([64, 65]);
    let original = |brush: &ImageBrush<FrameImage>, transform, metadata| {
        assert_eq!(
            quads(bounds, brush, transform, Affine::IDENTITY, layout, metadata)
                .iter()
                .collect::<Vec<_>>(),
            [bounds]
        );
    };
    original(&brush(None), Affine::IDENTITY, None);
    original(&brush(None), Affine::rotate(0.5), Some(&coverage));
    original(&brush(None), Affine::translate((-1., 0.)), Some(&coverage));
    original(
        &brush(Some(Rect::new(1., 1., 63., 64.))),
        Affine::IDENTITY,
        Some(&coverage),
    );
    let mut repeated = brush(None);
    repeated.sampler.x_extend = Extend::Repeat;
    original(&repeated, Affine::IDENTITY, Some(&coverage));
    let mut packed = brush(None);
    packed.image.appearance = ImageAppearance::new(
        None,
        ImageColorAdjustment::IDENTITY,
        ImageAlphaLayout::TopBottom,
    );
    original(&packed, Affine::IDENTITY, Some(&coverage));
    let mut styled = brush(None);
    styled.image.appearance = ImageAppearance::new(
        None,
        ImageColorAdjustment {
            offset: [0.5; 3],
            ..Default::default()
        },
        ImageAlphaLayout::Native,
    );
    assert!(
        quads(
            bounds,
            &styled,
            Affine::IDENTITY,
            Affine::IDENTITY,
            layout,
            Some(&coverage)
        )
        .iter()
        .map(|rect| rect.area())
        .sum::<f64>()
            < bounds.area()
    );
    let full = ImageCoverage::rgba8([64, 65], &vec![255; 64 * 65 * 4]).unwrap();
    original(&brush(None), Affine::IDENTITY, Some(&full));
    let empty = ImageCoverage::rgba8([64, 65], &vec![0; 64 * 65 * 4]).unwrap();
    assert_eq!(
        quads(
            bounds,
            &brush(None),
            Affine::IDENTITY,
            Affine::IDENTITY,
            layout,
            Some(&empty)
        )
        .iter()
        .count(),
        0
    );
}

#[test]
fn raster_border_protects_internal_transparent_edges_without_overlapping_bands() {
    let size = [64, 64];
    let mut pixels = vec![0; 64 * 64 * 4];
    pixels[(8 * 64 + 20) * 4 + 3] = 255;
    let coverage = ImageCoverage::rgba8(size, &pixels).unwrap();
    let shader = Point::new(19.6, 8.2);
    let physical = Affine::rotate(std::f64::consts::FRAC_PI_4);
    let sample = physical.inverse() * (physical * shader + reel_ui::kurbo::Vec2::new(-0.5, -0.5));
    let full = Rect::new(0., 0., 64., 64.);
    assert!(full.contains(sample));
    assert!(sampled_alpha(&pixels, size, shader, full, false) > 0.);
    assert!(
        !coverage.rectangles().any(|[x0, y0, x1, y1]| Rect::new(
            f64::from(x0),
            f64::from(y0),
            f64::from(x1),
            f64::from(y1)
        )
        .contains(sample)),
        "texel filtering border alone misses this interior MSAA sample"
    );
    let quads = quads(
        full,
        &brush(None),
        Affine::IDENTITY,
        physical,
        TextureLayout::identity(size),
        Some(&coverage),
    );
    assert!(quads.iter().any(|quad| quad.contains(sample)));
    assert_eq!(quads.iter().count(), 1, "identical padded bands merge");
}

#[test]
#[ignore = "explicit CPU geometry cost measurement; run in release with --nocapture"]
fn coverage_geometry_costs() {
    use std::{hint::black_box, time::Instant};
    let size = [1024, 1024];
    let mut pixels = vec![0; 1024 * 1024 * 4];
    for y in 64..1024usize {
        let half = if y < 512 { 100 } else { 100 + (y - 512) / 2 };
        for x in 512 - half..512 + half {
            pixels[(y * 1024 + x) * 4 + 3] = 255;
        }
    }
    let sparse = ImageCoverage::rgba8(size, &pixels).unwrap();
    let opaque = ImageCoverage::rgba8(size, &vec![255; pixels.len()]).unwrap();
    let empty = ImageCoverage::rgba8(size, &vec![0; pixels.len()]).unwrap();
    let bounds = Rect::new(0., 0., 1024., 1024.);
    let brush = brush(None);
    for (name, metadata) in [("sparse", &sparse), ("opaque", &opaque), ("empty", &empty)] {
        let plan = || {
            quads(
                bounds,
                &brush,
                Affine::IDENTITY,
                Affine::scale(544. / 1024.),
                TextureLayout::identity(size),
                Some(black_box(metadata)),
            )
        };
        let quads = plan();
        let area: f64 = quads.iter().map(|rect| rect.area()).sum();
        let start = Instant::now();
        for _ in 0..100_000 {
            black_box(plan());
        }
        eprintln!(
            "{name}: plan_us={:.3} quads={} vertices={} indices={} area={area:.3} original_area={} reduction_percent={:.3}",
            start.elapsed().as_secs_f64() * 10.,
            quads.len,
            quads.len * 4,
            quads.len * 6,
            bounds.area(),
            100. * (1. - area / bounds.area())
        );
    }
}
