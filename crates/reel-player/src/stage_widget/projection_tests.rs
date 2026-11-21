use super::*;
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    dpi::{PhysicalPosition, PhysicalSize},
    peniko::{Blob, ImageAlphaType, ImageFormat},
};
use std::time::Instant;

fn mount(view: &StageView) -> (RenderRoot, WidgetId) {
    let widget = StageWidget::new(Some(view)).prepare();
    let id = widget.id();
    (
        RenderRoot::new(
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
        ),
        id,
    )
}
fn sync(root: &mut RenderRoot, id: WidgetId, view: &StageView) {
    root.edit_widget(id, |mut widget| {
        StageWidget::sync(&mut widget.downcast::<StageWidget>(), view)
    });
}
fn scene(root: &RenderRoot, id: WidgetId) -> WidgetRef<'_, SceneWidget> {
    let stage = root
        .get_widget(id)
        .unwrap()
        .downcast::<StageWidget>()
        .unwrap();
    root.get_widget(stage.root.as_ref().unwrap().id())
        .unwrap()
        .downcast::<SceneWidget>()
        .unwrap()
}
fn scroll(offset: f64) -> NodeKind {
    NodeKind::Scroll {
        overscroll: Default::default(),
        axis: ScrollAxis::Vertical,
        follow_end: false,
        snap_y: None,
        align: reel_ui::widgets::ScrollAlign::Start,
        offset: Vec2::new(0., offset),
    }
}

#[test]
fn virtual_feedback_keeps_committed_topology_and_input_ownership() {
    let mut stage = Stage::default();
    let child = stage
        .create(
            NodeKind::Column(Default::default()),
            NodeProps::default(),
            vec![],
        )
        .unwrap();
    let root = stage
        .create(scroll(0.), NodeProps::default(), vec![child])
        .unwrap();
    stage.replace(root).unwrap();
    let view = StageView::capture(&stage);
    stage
        .update(root, scroll(25.), NodeProps::default())
        .unwrap();
    let mut props = stage.node(child).unwrap().props.clone();
    props.y = 15.;
    props.opacity = 0.75;
    stage
        .update(child, NodeKind::Column(Default::default()), props.clone())
        .unwrap();
    assert!(view.permits_feedback(&stage));
    for field in 0..3 {
        let mut changed = props.clone();
        match field {
            0 => changed.visible = false,
            1 => changed.enabled = false,
            _ => changed.modal = true,
        }
        stage
            .update(child, NodeKind::Column(Default::default()), changed)
            .unwrap();
        assert!(!view.permits_feedback(&stage));
    }
    stage
        .update(child, NodeKind::Row(Default::default()), props)
        .unwrap();
    assert!(!view.permits_feedback(&stage));
}

#[test]
#[ignore = "CPU allocation measurement; run alone with --ignored --exact --test-threads=1 --nocapture"]
fn scroll_projection_and_sync_share_ten_thousand_text_nodes() {
    let mut measurements = Vec::new();
    for count in [100, 10_000] {
        let mut stage = Stage::default();
        let children = (0..count)
            .map(|i| {
                stage
                    .create(
                        NodeKind::Text(format!("History paragraph {i}").into()),
                        NodeProps {
                            width: Some(600.),
                            height: Some(24.),
                            ..Default::default()
                        },
                        vec![],
                    )
                    .unwrap()
            })
            .collect();
        let id = stage
            .create(
                scroll(0.),
                NodeProps {
                    width: Some(640.),
                    height: Some(420.),
                    ..Default::default()
                },
                children,
            )
            .unwrap();
        stage.replace(id).unwrap();
        let mut view = StageView::capture(&stage);
        let original = view.root.as_ref().unwrap().clone();
        let (mut root, widget) = mount(&view);
        let start = Instant::now();
        let (stats, projection_time) = root.edit_widget(widget, |mut widget| {
            let mut widget = widget.downcast::<StageWidget>();
            let projection_start = Instant::now();
            let stats = allocation_counter::measure(|| {
                for index in 1..=200 {
                    stage
                        .update(
                            id,
                            scroll(index as f64),
                            stage.node(id).unwrap().props.clone(),
                        )
                        .unwrap();
                    let next = StageView::update(&stage, Some(&view));
                    StageWidget::sync(&mut widget, &next);
                    view = next;
                }
            });
            (stats, projection_start.elapsed())
        });
        let elapsed = start.elapsed();
        let current = view.root.as_ref().unwrap();
        assert!(Arc::ptr_eq(&original.children, &current.children));
        assert!(
            original
                .children
                .iter()
                .zip(current.children.iter())
                .all(|(a, b)| Arc::ptr_eq(a, b) && Arc::ptr_eq(&a.node, &b.node))
        );
        let viewport = scene(&root, widget);
        assert_eq!(viewport.syncs, 200);
        assert!(viewport.children.iter().all(|(_, child)| {
            root.get_widget(child.id())
                .unwrap()
                .downcast::<SceneWidget>()
                .unwrap()
                .syncs
                == 0
        }));
        eprintln!(
            "{count} text nodes, 200 scroll updates: projection/sync={projection_time:?}, including final UI passes={elapsed:?}; allocation_calls={}, allocated_bytes={}, unchanged text nodes cloned=0, child syncs=0",
            stats.count_total, stats.bytes_total
        );
        measurements.push(stats);
    }
    assert_eq!(measurements[0].count_total, measurements[1].count_total);
    assert_eq!(measurements[0].bytes_total, measurements[1].bytes_total);
}

#[test]
fn replacement_stage_with_matching_ids_and_revisions_replaces_widgets_and_button_lifetime() {
    fn stage(kind: NodeKind) -> Stage {
        let mut stage = Stage::default();
        let id = stage
            .create(
                kind,
                NodeProps {
                    width: Some(100.),
                    height: Some(40.),
                    ..Default::default()
                },
                vec![],
            )
            .unwrap();
        stage.replace(id).unwrap();
        stage
    }
    let first = stage(NodeKind::Button {
        label: Some("same".into()),
        listening: true,
        focus_on_pointer: true,
        focusable: true,
    });
    let second = stage(NodeKind::Button {
        label: Some("same".into()),
        listening: true,
        focus_on_pointer: true,
        focusable: true,
    });
    assert_eq!(
        (first.root(), first.revision()),
        (second.root(), second.revision())
    );
    let a = StageView::capture(&first);
    let b = StageView::update(&second, Some(&a));
    assert!(!a.matches(&second));
    assert!(!Arc::ptr_eq(
        a.root.as_ref().unwrap(),
        b.root.as_ref().unwrap()
    ));
    assert_eq!(
        b.button_changes(Some(&a), false),
        Some(vec![(second.root().unwrap(), true)])
    );
    let (mut root, widget) = mount(&a);
    let old = scene(&root, widget).id();
    sync(&mut root, widget, &b);
    assert_ne!(scene(&root, widget).id(), old);
    let third = stage(NodeKind::Slider {
        range: reel_ui::widgets::SliderRange::new(0., 1., None).unwrap(),
        value: 0.5,
        listening: true,
        art: Default::default(),
    });
    let c = StageView::update(&third, Some(&b));
    sync(&mut root, widget, &c);
    assert!(scene(&root, widget).slider.is_some());
}

#[test]
fn optimistic_control_proposal_is_reconciled_through_unchanged_ancestors() {
    let mut stage = Stage::default();
    let control = stage
        .create(
            NodeKind::Toggle {
                label: None,
                value: false,
                listening: true,
                appearance: reel_ui::stage::ToggleAppearance::Checkbox,
            },
            NodeProps {
                width: Some(100.),
                height: Some(40.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let inner = stage
        .create(NodeKind::Stack, NodeProps::default(), vec![control])
        .unwrap();
    let outer = stage
        .create(NodeKind::Stack, NodeProps::default(), vec![inner])
        .unwrap();
    stage.replace(outer).unwrap();
    let view = StageView::capture(&stage);
    let (mut root, widget) = mount(&view);
    let control_id = {
        let outer = scene(&root, widget);
        let inner = root
            .get_widget(outer.children[0].1.id())
            .unwrap()
            .downcast::<SceneWidget>()
            .unwrap();
        inner.children[0].1.id()
    };
    for down in [true, false] {
        let event = PointerButtonEvent {
            pointer: PointerInfo {
                pointer_id: Some(PointerId::PRIMARY),
                persistent_device_id: None,
                pointer_type: PointerType::Mouse,
            },
            state: PointerState {
                position: PhysicalPosition::new(20., 20.),
                ..Default::default()
            },
            button: Some(PointerButton::Primary),
        };
        root.handle_pointer_event(if down {
            PointerEvent::Down(event)
        } else {
            PointerEvent::Up(event)
        });
    }
    assert!(matches!(
        root.get_widget(control_id)
            .unwrap()
            .downcast::<SceneWidget>()
            .unwrap()
            .node
            .kind,
        NodeKind::Toggle { value: true, .. }
    ));
    assert!(
        scene(&root, widget).sync_dirty,
        "proposal bubbles to unchanged ancestors"
    );
    sync(&mut root, widget, &view);
    assert!(matches!(
        root.get_widget(control_id)
            .unwrap()
            .downcast::<SceneWidget>()
            .unwrap()
            .node
            .kind,
        NodeKind::Toggle { value: false, .. }
    ));
    assert!(!scene(&root, widget).sync_dirty);
    assert!(
        matches!(
            view.root.as_ref().unwrap().children[0].children[0]
                .node
                .kind,
            NodeKind::Toggle { value: false, .. }
        ),
        "optimistic edits must not mutate shared projections"
    );
}

#[test]
fn button_bindings_reuse_animation_and_track_content_topology_and_feedback() {
    let mut stage = Stage::default();
    let caption = stage
        .create(
            NodeKind::Text("Continue".into()),
            NodeProps::default(),
            vec![],
        )
        .unwrap();
    let button = stage
        .create(
            NodeKind::Button {
                label: None,
                listening: true,
                focus_on_pointer: true,
                focusable: true,
            },
            NodeProps::default(),
            vec![caption],
        )
        .unwrap();
    let other = stage
        .create(NodeKind::Stack, NodeProps::default(), vec![])
        .unwrap();
    let root = stage
        .create(NodeKind::Stack, NodeProps::default(), vec![button, other])
        .unwrap();
    stage.replace(root).unwrap();
    let original = StageView::capture(&stage);
    for id in [other, root, button, caption] {
        let node = stage.node(id).unwrap();
        let mut props = node.props.clone();
        props.x += 12.;
        props.opacity = 0.75;
        stage.update(id, node.kind.clone(), props).unwrap();
    }
    let animated = StageView::update(&stage, Some(&original));
    assert!(animated.button_changes(Some(&original), false).is_none());

    let props = stage.node(caption).unwrap().props.clone();
    stage
        .update(caption, NodeKind::Text("Replace".into()), props)
        .unwrap();
    let replaced = StageView::update(&stage, Some(&animated));
    assert_eq!(
        replaced.button_changes(Some(&animated), false),
        Some(vec![(button, true)])
    );
    assert!(replaced.button_changes(Some(&animated), true).is_none());

    stage.reconcile_children(root, vec![other, button]).unwrap();
    let reordered = StageView::update(&stage, Some(&replaced));
    assert_eq!(
        reordered.button_changes(Some(&replaced), false),
        Some(vec![(button, false)])
    );

    let node = stage.node(button).unwrap();
    let mut props = node.props.clone();
    props.enabled = false;
    stage.update(button, node.kind.clone(), props).unwrap();
    let disabled = StageView::update(&stage, Some(&reordered));
    assert_eq!(
        disabled.button_changes(Some(&reordered), false),
        Some(vec![(button, true)])
    );

    stage.remove(button).unwrap();
    let removed = StageView::update(&stage, Some(&disabled));
    assert_eq!(removed.button_changes(Some(&disabled), false), Some(vec![]));
}

#[test]
fn image_resolution_and_removal_invalidate_only_the_resource_branch() {
    use reel_core::{
        budget::{BudgetCategory, BudgetPool, QueueLimits},
        identity::{AssetKey, ContentVersion, ResourceVersion},
    };
    use reel_ui::resources::{ColorEncoding, ResourceRegistry};
    fn painted_images(root: &mut RenderRoot) -> Vec<reel_ui::resources::ResourceHandle> {
        root.redraw()
            .0
            .layers
            .iter()
            .flat_map(|layer| layer.command_owners())
            .filter_map(|owner| owner.image_resource.map(|resource| resource.resource))
            .collect()
    }
    let mut stage = Stage::default();
    let text = stage
        .create(
            NodeKind::Text("retained".into()),
            NodeProps::default(),
            vec![],
        )
        .unwrap();
    let picture = stage
        .create(
            NodeKind::Image {
                sampling: Default::default(),
                source_rect: None,
                source: "sprite.png".into(),
                origin: None,
                fit: Default::default(),
                slice: None,
                slice_scale: 1.,
                slice_center: true,
                repeat: [false; 2],
                tile_scale: 1.,
                corner_colors: None,
                corners: None,
                alpha_layout: Default::default(),
                adjustment: Default::default(),
            },
            NodeProps {
                mask: Some(reel_ui::stage::MaskProgram::image("mask.png".into())),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let container = stage
        .create(NodeKind::Stack, NodeProps::default(), vec![text, picture])
        .unwrap();
    stage.replace(container).unwrap();
    let mut view = StageView::capture(&stage);
    let image = ImageData {
        data: Blob::new(Arc::new(vec![255; 16])),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: 2,
        height: 2,
    };
    let mask_pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 1,
            max_bytes: 16,
        },
    );
    let mut masks = ResourceRegistry::new(1, 64 << 10).unwrap();
    let mask = masks
        .register(
            ResourceVersion {
                key: AssetKey("mask.png".into()),
                version: ContentVersion([1; 32]),
            },
            ResourceData::Image {
                image: image.clone(),
                encoding: ColorEncoding::Srgb,
            },
            mask_pool.reserve(16).unwrap(),
        )
        .unwrap();
    let mask = masks.acquire(mask).unwrap();
    let image_pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 2,
            max_bytes: 32,
        },
    );
    let mut images = ResourceRegistry::new(2, 64 << 10).unwrap();
    let original = images
        .register(
            ResourceVersion {
                key: AssetKey("sprite.png".into()),
                version: ContentVersion([2; 32]),
            },
            ResourceData::Image {
                image: image.clone(),
                encoding: ColorEncoding::Srgb,
            },
            image_pool.reserve(16).unwrap(),
        )
        .unwrap();
    let original = images.acquire(original).unwrap();
    resolve_image_nodes(&mut view.root, |node, _| {
        if node.node.id == picture {
            SceneImages {
                image_resource: Some(original.clone()),
                mask_images: vec![mask.clone()].into(),
                ..Default::default()
            }
        } else {
            SceneImages::default()
        }
    });
    let previous = view.clone();
    let (mut root, widget) = mount(&view);
    assert!(painted_images(&mut root).contains(&original.handle()));
    let pixels = Arc::new(vec![0; 16]);
    let released_pixels = Arc::downgrade(&pixels);
    let replacement_data = ImageData {
        data: Blob::new(pixels),
        ..image.clone()
    };
    let replacement = images
        .register(
            ResourceVersion {
                key: AssetKey("sprite.png".into()),
                version: ContentVersion([3; 32]),
            },
            ResourceData::Image {
                image: replacement_data,
                encoding: ColorEncoding::Srgb,
            },
            image_pool.reserve(16).unwrap(),
        )
        .unwrap();
    let replacement = images.acquire(replacement).unwrap();
    resolve_image_nodes(&mut view.root, |node, _| {
        if node.node.id == picture {
            SceneImages {
                image_resource: Some(replacement.clone()),
                mask_images: vec![mask.clone()].into(),
                ..Default::default()
            }
        } else {
            SceneImages::default()
        }
    });
    assert!(!Arc::ptr_eq(
        previous.root.as_ref().unwrap(),
        view.root.as_ref().unwrap()
    ));
    assert!(Arc::ptr_eq(
        &previous.root.as_ref().unwrap().children[0],
        &view.root.as_ref().unwrap().children[0]
    ));
    sync(&mut root, widget, &view);
    let parent = scene(&root, widget);
    let image_widget = root
        .get_widget(parent.children[1].1.id())
        .unwrap()
        .downcast::<SceneWidget>()
        .unwrap();
    assert_eq!(
        image_widget
            .projection
            .images
            .image_resource
            .as_ref()
            .unwrap()
            .handle(),
        replacement.handle()
    );
    assert_eq!(image_widget.syncs, 1);
    let image_id = image_widget.image.as_ref().unwrap().id();
    assert!(
        painted_images(&mut root).contains(&replacement.handle()),
        "same-size resource completion must replace cached image paint"
    );
    for visible in [false, true, false] {
        let node = stage.node(picture).unwrap();
        let mut props = node.props.clone();
        props.visible = visible;
        stage.update(picture, node.kind.clone(), props).unwrap();
        let mut next = StageView::update(&stage, Some(&view));
        if visible {
            resolve_image_nodes(&mut next.root, |node, _| {
                if node.node.id == picture {
                    SceneImages {
                        image_resource: Some(replacement.clone()),
                        mask_images: vec![mask.clone()].into(),
                        ..Default::default()
                    }
                } else {
                    SceneImages::default()
                }
            });
        } else {
            next.resolve_images(&BTreeMap::new(), EffectShaderFormat::Wgsl)
                .unwrap();
        }
        sync(&mut root, widget, &next);
        let parent = scene(&root, widget);
        let picture_widget = root
            .get_widget(parent.children[1].1.id())
            .unwrap()
            .downcast::<SceneWidget>()
            .unwrap();
        assert_eq!(picture_widget.image.as_ref().unwrap().id(), image_id);
        assert_eq!(
            painted_images(&mut root).contains(&replacement.handle()),
            visible
        );
        view = next;
    }
    assert!(images.retire(replacement.handle()));
    drop(replacement);
    assert!(
        released_pixels.upgrade().is_none(),
        "hidden widget and invalidated drawing cache release idle pixels"
    );
    stage
        .update(picture, NodeKind::Stack, NodeProps::default())
        .unwrap();
    let mut removed = StageView::update(&stage, Some(&view));
    removed
        .resolve_images(&BTreeMap::new(), EffectShaderFormat::Wgsl)
        .unwrap();
    let projected = &removed.root.as_ref().unwrap().children[1];
    assert!(!projected.image_branch);
    assert!(projected.images == SceneImages::default());
    sync(&mut root, widget, &removed);
    let parent = scene(&root, widget);
    let image_widget = root
        .get_widget(parent.children[1].1.id())
        .unwrap()
        .downcast::<SceneWidget>()
        .unwrap();
    assert!(image_widget.image.is_none() && image_widget.mask_images.is_empty());
}

#[test]
fn resolved_rgba_asset_records_one_resource_identity() {
    use reel_core::{
        budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
        identity::{AssetKey, ContentVersion, ResourceVersion},
    };
    use reel_ui::resources::{ColorEncoding, ResourceRegistry};
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 8,
            max_bytes: 1024,
        },
    );
    let mut registry = ResourceRegistry::new(1, 65536).unwrap();
    let handle = registry
        .register_image(
            ResourceVersion {
                key: AssetKey("image.png".into()),
                version: ContentVersion([7; 32]),
            },
            OwnedBytes::new(vec![255; 16], pool.reserve(16).unwrap()).unwrap(),
            [2, 2],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    let lease = registry.acquire(handle).unwrap();
    let mut stage = Stage::default();
    let picture = stage
        .create(
            NodeKind::Image {
                sampling: Default::default(),
                source_rect: None,
                source: "image.png".into(),
                origin: None,
                fit: Default::default(),
                slice: None,
                slice_scale: 1.,
                slice_center: true,
                repeat: [false; 2],
                tile_scale: 1.,
                corner_colors: None,
                corners: None,
                alpha_layout: Default::default(),
                adjustment: Default::default(),
            },
            NodeProps::default(),
            vec![],
        )
        .unwrap();
    stage.replace(picture).unwrap();
    let mut view = StageView::capture(&stage);
    view.resolve_images(
        &BTreeMap::from([("image.png".into(), lease)]),
        EffectShaderFormat::Wgsl,
    )
    .unwrap();
    let node = view.root.as_ref().unwrap();
    assert_eq!(
        node.images.image_resource.as_ref().unwrap().handle(),
        handle
    );
    let (mut root, _) = mount(&view);
    let layers = root.redraw().0.layers;
    assert!(
        layers
            .iter()
            .flat_map(|layer| layer.command_owners())
            .any(|owner| owner
                .image_resource
                .is_some_and(|resource| resource.resource == handle))
    );
}
