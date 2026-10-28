//! Compare recorded image sampling for inline and resource-owned pixels.
#[allow(dead_code)]
#[path = "support/fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/r01_fixture.rs"]
mod r01_fixture;
use r01_fixture::*;
use reel_core::identity::{AssetKey, ContentVersion, FrameId};
use reel_ui::{
    core::*,
    frame::*,
    imaging::record::{Geometry, Scene},
    kurbo::{Affine, Point, Rect, Size},
    peniko::{Blob, Brush},
    resources::*,
};
use std::sync::Arc;
fn version(n: u8) -> ResourceVersion {
    ResourceVersion {
        key: AssetKey(format!("resource/{n}")),
        version: ContentVersion([n; 32]),
    }
}
#[test]
fn image_resource_and_inline_image_emit_equivalent_sampling_geometry() {
    use reel_ui::{
        peniko::{ImageAlphaType, ImageData, ImageFormat},
        properties::ObjectFit,
        widgets::Image,
    };
    let pool = pool();
    let image = ImageData {
        data: Blob::new(Arc::new(vec![100; 64 * 32 * 4])),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: 64,
        height: 32,
    };
    let mut registry = ResourceRegistry::new(1, 65536).unwrap();
    let handle = registry
        .register(
            version(92),
            ResourceData::Image {
                image: image.clone(),
                encoding: ColorEncoding::Srgb,
            },
            pool.reserve((64 * 32 * 4) as u64).unwrap(),
        )
        .unwrap();
    for (source, slice, repeat) in [
        (None, None, [false, false]),
        (Some(Rect::new(4., 3., 44., 25.)), None, [false, false]),
        (
            None,
            Some(reel_ui::kurbo::Insets::uniform(4.)),
            [false, false],
        ),
        (None, None, [true, true]),
    ] {
        for fit in [ObjectFit::Contain, ObjectFit::Cover, ObjectFit::Stretch] {
            let mut frames = Vec::new();
            for widget_image in [
                Image::new(image.clone()),
                Image::from_resource(registry.acquire(handle).unwrap()),
            ] {
                let mut widget = RecordWidget::new(Scene::new());
                widget.clip = false;
                widget.inner.add(
                    widget_image
                        .with_sampling(reel_ui::peniko::ImageQuality::Low)
                        .with_source_rect(source)
                        .with_slice(slice, 1., true)
                        .with_pattern(repeat, 1., None)
                        .prepare()
                        .with_props(PropertySet::new().with(fit))
                        .with_transform(Affine::rotate(0.2)),
                    Point::new(20., 30.),
                    Size::new(100., 100.),
                );
                let mut bridge = widget_bridge(widget, Affine::IDENTITY);
                let frame = build(
                    bridge.record(FrameId(1)).unwrap(),
                    registry.freeze(&[handle], &pool).unwrap(),
                    &pool,
                );
                let draws: Vec<_> = frame
                    .commands()
                    .iter()
                    .filter_map(|command| match &command.op {
                        FrameOp::Draw(FrameDraw::Fill {
                            transform,
                            shape: Geometry::Rect(rect),
                            brush: Brush::Image(brush),
                            brush_transform,
                            ..
                        }) => Some((
                            *transform,
                            *rect,
                            *brush_transform,
                            brush.sampler,
                            brush.image,
                        )),
                        _ => None,
                    })
                    .collect();
                frames.push(draws);
            }
            assert!(!frames[0].is_empty());
            assert_eq!(
                frames[0], frames[1],
                "source={source:?} slice={slice:?} repeat={repeat:?} fit={fit:?}"
            );
        }
    }
}
