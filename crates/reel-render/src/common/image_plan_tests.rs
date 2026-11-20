use super::*;
use crate::test_support::r01_fixture;
use r01_fixture::*;
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
};
use reel_ui::{
    core::PaintImageMask,
    frame::{FrameBuild, FrameInfo, FrameTransition, RetainBuild},
    imaging::record::Scene,
    kurbo::{Affine, Rect},
    peniko::{ImageAlphaType, ImageFormat},
    resources::{ColorEncoding, ResourceRegistry, ResourceSnapshot},
};

fn image(
    registry: &mut ResourceRegistry,
    key: u8,
    pool: &reel_core::budget::BudgetPool,
) -> ResourceLease {
    let handle = registry
        .register_image(
            ResourceVersion {
                key: AssetKey(format!("image-{key}")),
                version: ContentVersion([key; 32]),
            },
            OwnedBytes::new(vec![255; 4], pool.reserve(4).unwrap()).unwrap(),
            [1, 1],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    registry.acquire(handle).unwrap()
}
fn widget(source: &ResourceLease) -> RecordWidget {
    let mut widget = RecordWidget::new(Scene::new());
    widget.clip = false;
    widget.resource_draws.push((
        source.clone(),
        Rect::new(0., 0., 400., 300.),
        Affine::IDENTITY,
        Default::default(),
    ));
    widget
}

#[test]
fn demand_omits_unused_snapshot_images_and_preserves_each_command_location() {
    let pool = pool();
    let mut registry = ResourceRegistry::new(3, 65536).unwrap();
    let drawn = image(&mut registry, 1, &pool);
    let mask = image(&mut registry, 2, &pool);
    let unused = image(&mut registry, 3, &pool);
    let mut widget = widget(&drawn);
    widget.mask = Some(PaintImageMask {
        program: reel_ui::stage::MaskProgram::image("mask".into()),
        images: vec![mask.clone()].into(),
        bounds: Rect::new(0., 0., 400., 300.),
        parameters: [0.5, 0., 0., 0., 0.],
    });
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        ResourceSnapshot::capture(
            [drawn.clone(), mask.clone(), unused.clone()]
                .into_iter()
                .map(Ok),
            &pool,
        )
        .unwrap(),
        &pool,
    );
    assert_eq!(frame.resources().leases().len(), 3);
    let plan = collect(
        std::iter::once(&frame),
        4096,
        false,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(plan.images.len(), 2);
    assert_eq!(plan.masks.len(), 1);
    for required in &plan.images {
        let command = frame
            .commands()
            .iter()
            .find(|command| match &command.op {
                FrameOp::Draw(FrameDraw::Fill {
                    brush: Brush::Image(brush),
                    ..
                }) => brush.image.resource == required.source.handle(),
                FrameOp::PushGroup(group) => group
                    .image_mask
                    .as_ref()
                    .is_some_and(|mask| mask.images.contains(&required.source.handle())),
                _ => false,
            })
            .unwrap();
        assert_eq!(required.at, command.origin.at);
        assert_ne!(required.source.handle(), unused.handle());
    }
}

#[test]
fn capture_demand_omits_excluded_commands_and_their_masks() {
    let pool = pool();
    let mut registry = ResourceRegistry::new(1, 65536).unwrap();
    let source = image(&mut registry, 1, &pool);
    let mut widget = widget(&source);
    widget.capture_included = false;
    widget.mask = Some(PaintImageMask {
        program: reel_ui::stage::MaskProgram::image("mask".into()),
        images: vec![source.clone()].into(),
        bounds: Rect::new(0., 0., 400., 300.),
        parameters: [0.5, 0., 0., 0., 0.],
    });
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        ResourceSnapshot::capture([Ok(source)].into_iter(), &pool).unwrap(),
        &pool,
    );
    let normal = collect(
        std::iter::once(&frame),
        4096,
        false,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(normal.images.len(), 1);
    assert_eq!(normal.masks.len(), 1);
    let capture = collect(
        std::iter::once(&frame),
        4096,
        true,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    assert!(capture.images.is_empty());
    assert!(capture.masks.is_empty());
}

#[test]
fn transition_zero_demand_includes_previous_target_and_mask_without_unused_leases() {
    let pool = pool();
    let mut registry = ResourceRegistry::new(4, 65536).unwrap();
    let images = (1..=4)
        .map(|key| image(&mut registry, key, &pool))
        .collect::<Vec<_>>();
    let mut bridge = widget_bridge(widget(&images[0]), Affine::IDENTITY);
    let old = build(
        bridge.record(FrameId(1)).unwrap(),
        ResourceSnapshot::capture([Ok(images[0].clone())].into_iter(), &pool).unwrap(),
        &pool,
    );
    let mut retain = RetainBuild::new(&old, None, limits(), &pool).unwrap();
    while !retain.step(step_budget()).unwrap().complete {}
    let previous = retain.finish().unwrap();
    let mut bridge = widget_bridge(widget(&images[1]), Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(2)).unwrap(),
        ResourceSnapshot::capture(images[1..].iter().cloned().map(Ok), &pool).unwrap(),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        limits(),
        &pool,
    )
    .unwrap();
    build
        .set_transitions(vec![FrameTransition {
            root: None,
            previous,
            progress: 0.,
            mask: Some(PaintImageMask {
                program: reel_ui::stage::MaskProgram::image("mask".into()),
                images: vec![images[2].clone()].into(),
                bounds: Rect::new(0., 0., 400., 300.),
                parameters: [0., 0., 0., 0., 0.],
            }),
        }])
        .unwrap();
    while !build.step(step_budget()).unwrap().complete {}
    let frame = build.finish().unwrap();
    let plan = collect(
        std::iter::once(&frame),
        4096,
        false,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    let handles = plan
        .images
        .iter()
        .map(|item| item.source.handle())
        .collect::<HashSet<_>>();
    assert_eq!(
        handles,
        images[..3].iter().map(ResourceLease::handle).collect()
    );
    assert_eq!(plan.masks.len(), 1);
}

#[test]
fn missing_pixel_demand_retains_only_absent_sources_and_charges_capacity() {
    let pool = pool();
    let mut registry = ResourceRegistry::new(2, 65536).unwrap();
    let present = image(&mut registry, 1, &pool);
    let missing = image(&mut registry, 2, &pool);
    let before = pool.usage().bytes;
    assert!(
        missing_pixels([&present, &missing].into_iter(), &pool, u64::MAX)
            .unwrap()
            .is_none()
    );
    assert_eq!(pool.usage().bytes, before);
    missing.permit_pixel_reload(None);
    assert!(missing.discard_uploaded_pixels());
    let before = pool.usage().bytes;
    let Err(GpuError::PixelsNeeded(single)) = required_pixels(&missing, &pool, u64::MAX) else {
        panic!("an absent upload reader requests its original source");
    };
    assert_eq!(single.resources()[0].handle(), missing.handle());
    drop(single);
    assert_eq!(pool.usage().bytes, before);
    let demand = missing_pixels([&present, &missing].into_iter(), &pool, u64::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(demand.resources().len(), 1);
    assert_eq!(demand.resources()[0].handle(), missing.handle());
    assert!(pool.usage().bytes > before);
    drop(demand);
    assert_eq!(pool.usage().bytes, before);
}
