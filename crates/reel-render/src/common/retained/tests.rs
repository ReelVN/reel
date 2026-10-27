use super::*;
use crate::{
    common::{glyph_plan, image_plan, target_plan},
    test_support::{fixture, r01_fixture as f},
};
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
};
use reel_ui::{
    bridge::PresentationBridge,
    core::{Widget, WidgetId},
    frame::{
        FrameBuild, FrameGlyphRun, FrameInfo, FrameTransition, InkBounds, RecordLocation,
        RetainBuild,
    },
    imaging::{
        Composite,
        record::{Draw, Geometry, Scene},
    },
    kurbo::{Affine, Point, Rect, Size},
    peniko::{Brush, Color, ImageAlphaType, ImageFormat},
    resources::{ColorEncoding, ResourceLease, ResourceRegistry, ResourceSnapshot},
};

fn retained(frame: &RenderFrame, pool: &BudgetPool) -> RetainedScene {
    let mut build = RetainBuild::new(frame, None, f::limits(), pool).unwrap();
    let mut budget = f::step_budget();
    loop {
        let progress = build.step(budget).unwrap();
        if progress.complete {
            return build.finish().unwrap();
        }
        budget.max_units = budget.max_units.max(progress.minimum_units);
        budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
    }
}
fn build(
    bridge: &mut PresentationBridge,
    id: u64,
    target: Option<WidgetId>,
    previous: &RetainedScene,
    resources: ResourceSnapshot,
    pool: &BudgetPool,
) -> RenderFrame {
    let mut build = FrameBuild::new(
        bridge.record(FrameId(id)).unwrap(),
        resources,
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        f::limits(),
        pool,
    )
    .unwrap();
    build
        .set_transitions(vec![FrameTransition {
            root: target,
            previous: previous.clone(),
            progress: 0.5,
            mask: None,
        }])
        .unwrap();
    let mut budget = f::step_budget();
    loop {
        let progress = build.step(budget).unwrap();
        if progress.complete {
            return build.finish().unwrap();
        }
        budget.max_units = budget.max_units.max(progress.minimum_units);
        budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
    }
}
fn plan(frame: &RenderFrame, cache: &Cache<(), u8>, budgets: &Budgets) -> Plan {
    Plan::collect(
        std::iter::once(frame),
        [400, 300],
        false,
        budgets,
        &Budgets::new(Default::default()),
        32 << 20,
        |key| cache.lookup(key, 0, &frame.key()),
        |_| Ok(()),
    )
    .unwrap()
}
fn fill(color: Color) -> Scene {
    let mut scene = Scene::new();
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Solid(color),
        brush_transform: None,
        shape: Geometry::Rect(Rect::new(0., 0., 400., 300.)),
        composite: Composite::default(),
    });
    scene
}
fn old(pool: &BudgetPool) -> RetainedScene {
    let mut bridge = f::widget_bridge(f::RecordWidget::new(fill(Color::WHITE)), Affine::IDENTITY);
    let frame = f::build(
        bridge.record(FrameId(1)).unwrap(),
        f::empty_snapshot(pool),
        pool,
    );
    retained(&frame, pool)
}

#[test]
fn ordinary_frames_do_not_allocate_a_retained_plan() {
    let pool = f::pool();
    let mut bridge = f::widget_bridge(f::RecordWidget::new(fill(Color::WHITE)), Affine::IDENTITY);
    let frame = f::build(
        bridge.record(FrameId(1)).unwrap(),
        f::empty_snapshot(&pool),
        &pool,
    );
    let budgets = Budgets::new(Default::default());
    let plan = Plan::collect(
        std::iter::once(&frame),
        [400, 300],
        false,
        &budgets,
        &budgets,
        32 << 20,
        |_| panic!("ordinary frame has no retained lookup"),
        |_| panic!("ordinary frame has no retained metadata"),
    )
    .unwrap();
    assert!(!plan.enabled);
    assert_eq!(plan.entries.capacity(), 0);
    assert_eq!(plan.spans.capacity(), 0);
}

#[test]
fn an_earlier_empty_transition_mask_mapping_invalidates_the_later_backdrop() {
    let pool = f::pool();
    let previous = old(&pool);
    let mut parent = f::RecordWidget::new(fill(Color::WHITE));
    let mut targets = Vec::new();
    for x in [0., 80.] {
        let mut child = f::RecordWidget::new(Scene::new());
        child.clip = false;
        let child = child.prepare();
        targets.push(child.id());
        parent
            .inner
            .add(child, Point::new(x, 0.), Size::new(40., 40.));
    }
    let mut bridge = f::widget_bridge(parent, Affine::IDENTITY);
    let mask = reel_ui::core::PaintImageMask {
        program: reel_ui::stage::MaskProgram::new(
            vec![reel_ui::stage::MaskOp::Constant(0.5f32.to_bits())],
            vec![],
        )
        .unwrap(),
        images: vec![].into(),
        bounds: Rect::new(0., 0., 40., 40.),
        parameters: [0.; 5],
    };
    let build_pair = |bridge: &mut PresentationBridge, id| {
        let mut builder = FrameBuild::new(
            bridge.record(FrameId(id)).unwrap(),
            f::empty_snapshot(&pool),
            FrameInfo {
                logical_size: [400., 300.],
                physical_size: [400, 300],
            },
            f::limits(),
            &pool,
        )
        .unwrap();
        builder
            .set_transitions(
                targets
                    .iter()
                    .enumerate()
                    .map(|(index, target)| FrameTransition {
                        root: Some(*target),
                        previous: previous.clone(),
                        progress: 0.5,
                        mask: (index == 0).then(|| mask.clone()),
                    })
                    .collect(),
            )
            .unwrap();
        let mut budget = f::step_budget();
        loop {
            let progress = builder.step(budget).unwrap();
            if progress.complete {
                return builder.finish().unwrap();
            }
            budget.max_units = budget.max_units.max(progress.minimum_units);
            budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
        }
    };
    let budgets = Budgets::new(Default::default());
    let all_hot = |frame: &RenderFrame| {
        Plan::collect(
            std::iter::once(frame),
            [400, 300],
            false,
            &budgets,
            &Budgets::new(Default::default()),
            32 << 20,
            |key| {
                Some(Input {
                    key: key.clone(),
                    bytes: 400 * 300,
                    token: key.anchor,
                })
            },
            |_| Ok(()),
        )
        .unwrap()
    };
    let a = build_pair(&mut bridge, 1);
    assert!(
        a.drawing()
            .transitions()
            .iter()
            .all(|transition| transition.commands.is_empty())
    );
    let a = all_hot(&a);
    bridge.edit_widget(targets[0], |mut widget| {
        widget.ctx.set_transform(Affine::translate((20., 0.)))
    });
    let b = build_pair(&mut bridge, 2);
    let b = all_hot(&b);
    assert_eq!(a.entries[1].key.recordings, b.entries[1].key.recordings);
    assert!(
        a.entries[1].key != b.entries[1].key,
        "equal command prefixes still depend on earlier transition mask-to-window mapping"
    );
}

#[test]
fn unchanged_prefix_hits_while_parent_paint_transform_and_clip_invalidate() {
    let pool = f::pool();
    let previous = old(&pool);
    let child = f::RecordWidget::new(Scene::new()).prepare();
    let target = child.id();
    let mut parent = f::RecordWidget::new(fill(Color::WHITE));
    parent.inner.add(child, Point::ZERO, Size::new(400., 300.));
    let parent = parent.prepare();
    let root = parent.id();
    let mut bridge = f::bridge(fixture::root(parent).0);
    let budgets = Budgets::new(Default::default());
    let cache = Cache::default();
    let first = build(
        &mut bridge,
        1,
        Some(target),
        &previous,
        f::empty_snapshot(&pool),
        &pool,
    );
    let cold = plan(&first, &cache, &budgets);
    let key = cold.entries[cold.warm.unwrap()].key.clone();
    let token = cache
        .start(key.clone(), 0, first.key(), &budgets.transient, 32 << 20)
        .unwrap();
    cache.stage(token, (), 400 * 300 * 4);
    cache.complete(token);
    let stable = build(
        &mut bridge,
        2,
        Some(target),
        &previous,
        f::empty_snapshot(&pool),
        &pool,
    );
    let hot = plan(&stable, &cache, &budgets);
    assert!(hot.warm.is_none());
    assert!(hot.has_cached());
    assert!(
        !key.recordings.is_empty(),
        "live prefix geometry uses immutable recording identities"
    );
    bridge.edit_widget(root, |mut widget| {
        widget.ctx.set_transform(Affine::translate((13., 7.)))
    });
    let moved = build(
        &mut bridge,
        3,
        Some(target),
        &previous,
        f::empty_snapshot(&pool),
        &pool,
    );
    assert!(plan(&moved, &cache, &budgets).warm.is_some());
    bridge.edit_widget(root, |mut widget| {
        let mut widget = widget.downcast::<f::RecordWidget>();
        widget.widget.inner.clip = Rect::new(20., 10., 300., 200.);
        widget.ctx.request_layout();
    });
    let clipped = build(
        &mut bridge,
        4,
        Some(target),
        &previous,
        f::empty_snapshot(&pool),
        &pool,
    );
    let clipped = plan(&clipped, &cache, &budgets);
    assert!(clipped.entries[0].key != key);
    drop(first);
    drop(stable);
    drop(moved);
    bridge.edit_widget(root, |mut widget| {
        let mut widget = widget.downcast::<f::RecordWidget>();
        widget.widget.own = fill(Color::from_rgb8(20, 40, 80));
        widget.ctx.request_render();
    });
    let painted = build(
        &mut bridge,
        5,
        Some(target),
        &previous,
        f::empty_snapshot(&pool),
        &pool,
    );
    let painted = plan(&painted, &cache, &budgets);
    assert!(painted.entries[0].key != clipped.entries[0].key);
}

fn image(registry: &mut ResourceRegistry, byte: u8, pool: &BudgetPool) -> ResourceLease {
    let handle = registry
        .register_image(
            ResourceVersion {
                key: AssetKey(format!("retained-{byte}")),
                version: ContentVersion([byte; 32]),
            },
            OwnedBytes::new(vec![byte; 4], pool.reserve(4).unwrap()).unwrap(),
            [1, 1],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    registry.acquire(handle).unwrap()
}
fn image_widget(image: &ResourceLease) -> f::RecordWidget {
    let mut widget = f::RecordWidget::new(Scene::new());
    widget.clip = false;
    widget.resource_draws.push((
        image.clone(),
        Rect::new(0., 0., 400., 300.),
        Affine::IDENTITY,
        Default::default(),
    ));
    widget
}

#[test]
fn cold_frontier_requests_old_images_and_hot_input_requests_only_new_images() {
    let pool = f::pool();
    let mut registry = ResourceRegistry::new(2, 65536).unwrap();
    let old_image = image(&mut registry, 1, &pool);
    let new_image = image(&mut registry, 2, &pool);
    let mut old_bridge = f::widget_bridge(image_widget(&old_image), Affine::IDENTITY);
    let old_frame = f::build(
        old_bridge.record(FrameId(1)).unwrap(),
        ResourceSnapshot::capture([Ok(old_image.clone())].into_iter(), &pool).unwrap(),
        &pool,
    );
    let previous = retained(&old_frame, &pool);
    let mut bridge = f::widget_bridge(image_widget(&new_image), Affine::IDENTITY);
    let frame = build(
        &mut bridge,
        2,
        None,
        &previous,
        ResourceSnapshot::capture([Ok(new_image.clone())].into_iter(), &pool).unwrap(),
        &pool,
    );
    let budgets = Budgets::new(Default::default());
    let cache = Cache::default();
    let cold = plan(&frame, &cache, &budgets);
    let demand =
        image_plan::collect(std::iter::once(&frame), 4096, false, &cold, |_| Ok(())).unwrap();
    assert_eq!(
        demand
            .images
            .iter()
            .map(|image| image.source.handle())
            .collect::<Vec<_>>(),
        [old_image.handle()]
    );
    let cold_pixels = target_plan::target_pixels(
        &frame,
        None,
        [400, 300],
        false,
        1,
        4096,
        &glyph_plan::Plan::default(),
        &crate::common::offscreen::Plan::default(),
        target_plan::MaskExecution::Inline,
        &cold,
        |_| Ok(()),
    )
    .unwrap();
    let index = cold.warm.unwrap();
    let token = cache
        .start(
            cold.entries[index].key.clone(),
            0,
            frame.key(),
            &budgets.transient,
            32 << 20,
        )
        .unwrap();
    cache.stage(token, (), 400 * 300 * 4);
    cache.complete(token);
    let hot = plan(&frame, &cache, &budgets);
    let demand =
        image_plan::collect(std::iter::once(&frame), 4096, false, &hot, |_| Ok(())).unwrap();
    assert_eq!(
        demand
            .images
            .iter()
            .map(|image| image.source.handle())
            .collect::<Vec<_>>(),
        [new_image.handle()]
    );
    let hot_pixels = target_plan::target_pixels(
        &frame,
        None,
        [400, 300],
        false,
        1,
        4096,
        &glyph_plan::Plan::default(),
        &crate::common::offscreen::Plan::default(),
        target_plan::MaskExecution::Inline,
        &hot,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(cold_pixels, 2 * 400 * 300);
    assert_eq!(
        hot_pixels,
        3 * 400 * 300,
        "cached input and both new-side targets remain simultaneously live"
    );
}

#[test]
fn cancelled_pending_tokens_cannot_complete_replacement_outputs() {
    let pool = f::pool();
    let previous = old(&pool);
    let mut bridge = f::widget_bridge(f::RecordWidget::new(Scene::new()), Affine::IDENTITY);
    let a = build(
        &mut bridge,
        1,
        None,
        &previous,
        f::empty_snapshot(&pool),
        &pool,
    );
    let b = build(
        &mut bridge,
        2,
        None,
        &previous,
        f::empty_snapshot(&pool),
        &pool,
    );
    let budgets = Budgets::new(Default::default());
    let plan = plan(&a, &Cache::default(), &budgets);
    let key = plan.entries[0].key.clone();
    let cache = Cache::<Arc<BudgetTicket>, u8>::default();
    let first = cache
        .start(key.clone(), 0, a.key(), &budgets.transient, 32 << 20)
        .unwrap();
    let flight = Arc::new(budgets.targets.reserve(4096).unwrap());
    cache.stage(first, flight.clone(), 1024);
    assert!(matches!(
        cache.start(key.clone(), 0, b.key(), &budgets.transient, 32 << 20),
        Err(GpuError::Busy)
    ));
    cache.cancel(&a.key());
    assert_eq!(budgets.targets.usage().bytes, 4096);
    let second = cache
        .start(key.clone(), 0, b.key(), &budgets.transient, 32 << 20)
        .unwrap();
    assert_ne!(first, second);
    cache.stage(
        second,
        Arc::new(budgets.targets.reserve(4096).unwrap()),
        1024,
    );
    cache.complete(first);
    assert!(cache.pending(&b.key()));
    assert!(cache.lookup(&key, 0, &b.key()).is_none());
    drop(flight);
    assert_eq!(budgets.targets.usage().bytes, 4096);
    cache.complete(second);
    assert!(cache.lookup(&key, 0, &b.key()).is_some());
    cache.clear();
    assert_eq!(budgets.targets.usage().bytes, 0);
}

#[test]
fn cached_old_drawing_requests_no_glyph_raster_or_atlas_work() {
    use reel_ui::{
        app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
        core::{DefaultProperties, NewWidget},
        dpi::PhysicalSize,
        peniko::Blob,
        resources::ResourceData,
        widgets::Label,
    };
    struct Glyphs(usize);
    impl glyph_plan::Prepare for Glyphs {
        fn finish(&mut self) -> Result<(), GpuError> {
            Ok(())
        }
        fn reserve(&mut self, _: u64) -> Result<(), GpuError> {
            Ok(())
        }
        fn run(
            &mut self,
            _: &FrameDrawing,
            run: &FrameGlyphRun,
            _: f64,
            _: RecordLocation,
        ) -> Result<InkBounds, GpuError> {
            self.0 += run.glyphs.len();
            Ok(InkBounds::Empty)
        }
    }
    let pool = f::pool();
    let font = Blob::from(
        include_bytes!(
            "../../../../../examples/author_arcade/resources/fonts/AtkinsonHyperlegible-Regular.ttf"
        )
        .to_vec(),
    );
    let mut registry = ResourceRegistry::new(1, 65536).unwrap();
    let handle = registry
        .register(
            ResourceVersion {
                key: AssetKey("retained-font".into()),
                version: ContentVersion([8; 32]),
            },
            ResourceData::Font(font.clone()),
            pool.reserve(font.len() as u64).unwrap(),
        )
        .unwrap();
    let root = RenderRoot::new(
        NewWidget::new(Label::new("frozen glyphs")),
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(400, 300),
            scale_factor: 1.,
            default_font: Some(font),
        },
    );
    let mut bridge = f::bridge(root);
    let old_frame = f::build(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[handle], &pool).unwrap(),
        &pool,
    );
    let previous = retained(&old_frame, &pool);
    let mut bridge = f::widget_bridge(f::RecordWidget::new(Scene::new()), Affine::IDENTITY);
    let frame = build(
        &mut bridge,
        2,
        None,
        &previous,
        f::empty_snapshot(&pool),
        &pool,
    );
    let budgets = Budgets::new(Default::default());
    let cache = Cache::default();
    let cold = plan(&frame, &cache, &budgets);
    let mut glyphs = Glyphs(0);
    glyph_plan::Plan::collect(
        std::iter::once(&frame),
        Affine::IDENTITY,
        4096,
        false,
        &cold,
        &mut glyphs,
    )
    .unwrap();
    assert!(glyphs.0 > 0);
    let token = cache
        .start(
            cold.entries[cold.warm.unwrap()].key.clone(),
            0,
            frame.key(),
            &budgets.transient,
            32 << 20,
        )
        .unwrap();
    cache.stage(token, (), 400 * 300 * 4);
    cache.complete(token);
    let hot = plan(&frame, &cache, &budgets);
    let mut glyphs = Glyphs(0);
    glyph_plan::Plan::collect(
        std::iter::once(&frame),
        Affine::IDENTITY,
        4096,
        false,
        &hot,
        &mut glyphs,
    )
    .unwrap();
    assert_eq!(
        glyphs.0, 0,
        "cached pixels replace old text's GPU demand while its CPU font lease remains retained"
    );
}
