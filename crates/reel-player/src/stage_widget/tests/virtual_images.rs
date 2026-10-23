use super::*;
use reel_ui::stage::{VirtualListView, VirtualRow};

fn fixture() -> (Fixture, NodeId, NodeId, NodeId) {
    fixture_with_overflow(None)
}
fn fixture_with_overflow(descendant: Option<bool>) -> (Fixture, NodeId, NodeId, NodeId) {
    let (mut f, list, first) = virtual_fixture(100);
    let mut rows = vec![VirtualRow {
        node: first,
        index: 0,
        offset: 0.,
    }];
    for index in 1..6 {
        let row = f
            .stage
            .create(
                NodeKind::Button {
                    label: (index == 5 && descendant == Some(false)).then(|| "old".into()),
                    listening: true,
                },
                NodeProps {
                    height: Some(40.),
                    y: if index == 5 && descendant == Some(false) {
                        -210.
                    } else {
                        0.
                    },
                    ..Default::default()
                },
                vec![],
            )
            .unwrap();
        if index == 5 && descendant == Some(true) {
            let overflow = f
                .stage
                .create(
                    NodeKind::Button {
                        label: Some("old".into()),
                        listening: true,
                    },
                    NodeProps {
                        y: -210.,
                        width: Some(60.),
                        height: Some(40.),
                        ..Default::default()
                    },
                    vec![],
                )
                .unwrap();
            f.stage.add(row, overflow, 0).unwrap();
        }
        f.stage.add(list, row, index).unwrap();
        rows.push(VirtualRow {
            node: row,
            index,
            offset: index as f64 * 42.,
        });
    }
    let props = f.stage.node(list).unwrap().props.clone();
    f.stage
        .update(
            list,
            NodeKind::VirtualList(VirtualListView {
                revision: 1,
                total_height: 4200.,
                total_rows: 100,
                rows: rows.clone().into(),
                navigation: None,
            }),
            props.clone(),
        )
        .unwrap();
    let initial = f.record();
    f.accept(&initial);
    f.ui.set_assets(crate::assets::Assets::with_adapters(
        Default::default(),
        |wake| {
            let limits = QueueLimits {
                max_requests: 32,
                max_bytes: 768 << 20,
            };
            (
                Box::new(reel_core::test_support::MemoryAssetIo::new(
                    limits,
                    limits,
                    wake.clone(),
                )),
                Box::new(crate::assets::ImageExecutor::new(limits, wake)),
            )
        },
    ));
    // The host deliberately withholds this new cache row's image completion.
    let waiting = f
        .stage
        .create(
            NodeKind::Image {
                source: "waiting.png".into(),
                fit: reel_ui::properties::ObjectFit::Contain,
                slice: None,
                slice_scale: 1.,
                repeat: [false; 2],
                tile_scale: 1.,
                corner_colors: None,
                adjustment: Default::default(),
                slice_center: true,
                source_rect: None,
                origin: Some("retained.aside:8:5".into()),
            },
            NodeProps {
                width: Some(180.),
                height: Some(40.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    f.stage.add(list, waiting, 6).unwrap();
    rows.push(VirtualRow {
        node: waiting,
        index: 6,
        offset: 252.,
    });
    f.stage
        .update(
            list,
            NodeKind::VirtualList(VirtualListView {
                revision: 2,
                total_height: 4200.,
                total_rows: 100,
                rows: rows.into(),
                navigation: None,
            }),
            props,
        )
        .unwrap();
    (f, list, first, waiting)
}

#[test]
fn current_candidate_checks_old_new_transformed_and_overflowing_rows() {
    for case in [
        "old-row",
        "old-child",
        "moved-row",
        "moved-child",
        "new-row",
        "new-child",
    ] {
        let descendant = match case {
            "old-row" => Some(false),
            "old-child" => Some(true),
            _ => None,
        };
        let (mut f, list, _, waiting) = fixture_with_overflow(descendant);
        let old = f.record();
        f.accept(&old);
        let image = f.stage.node(waiting).unwrap().kind.clone();
        match case {
            "old-row" | "old-child" => {
                let changed = f
                    .stage
                    .nodes()
                    .find_map(|node| {
                        matches!(&node.kind,NodeKind::Button{label:Some(label),..} if label=="old")
                            .then_some(node.id)
                    })
                    .unwrap();
                let mut props = f.stage.node(changed).unwrap().props.clone();
                props.width = Some(60.);
                f.stage
                    .update(
                        changed,
                        NodeKind::Button {
                            label: Some("new".into()),
                            listening: true,
                        },
                        props.clone(),
                    )
                    .unwrap();
                let latest = f.record();
                f.accept(&latest);
                assert!(!f.ui.images_block_presentation());
                let widget = f
                    .bridge
                    .root()
                    .get_widget(f.layouts()[&changed].0)
                    .unwrap()
                    .downcast::<SceneWidget>()
                    .unwrap();
                assert!(
                    matches!(&widget.node.kind,NodeKind::Button{label:Some(label),..} if label=="new")
                );
                f.stage.update(changed, image, props).unwrap();
            }
            "moved-row" | "moved-child" => {
                let row = f.stage.node(list).unwrap().children[5];
                if case == "moved-row" {
                    let mut props = f.stage.node(row).unwrap().props.clone();
                    props.y = -210.;
                    props.width = Some(60.);
                    f.stage.update(row, image, props).unwrap();
                } else {
                    let child = f
                        .stage
                        .create(
                            image,
                            NodeProps {
                                y: -210.,
                                width: Some(60.),
                                height: Some(40.),
                                ..Default::default()
                            },
                            vec![],
                        )
                        .unwrap();
                    f.stage.add(row, child, 0).unwrap();
                }
            }
            "new-row" => f.edit(waiting, |props| props.y = -252.),
            "new-child" => {
                let props = f.stage.node(waiting).unwrap().props.clone();
                f.stage.update(waiting, NodeKind::Stack, props).unwrap();
                let child = f
                    .stage
                    .create(
                        image,
                        NodeProps {
                            y: -252.,
                            width: Some(60.),
                            height: Some(40.),
                            ..Default::default()
                        },
                        vec![],
                    )
                    .unwrap();
                f.stage.add(waiting, child, 0).unwrap();
            }
            _ => unreachable!(),
        }
        f.ui.set_stage(&f.stage);
        f.ui.sync(&mut f.bridge, f.widget).unwrap();
        assert!(f.ui.images_block_presentation(), "{case}");
    }
}

#[test]
fn virtual_resources_use_current_content_and_wait_on_scope_unknown_size_and_uncovered_ranges() {
    for case in [
        "unchanged",
        "width",
        "appearance",
        "content",
        "reorder",
        "far",
        "modal",
        "outside",
        "unknown-size",
        "outside-image",
    ] {
        let (mut f, list, first, waiting) = fixture();
        match case {
            "width" => f.edit(f.root, |props| props.width = Some(120.)),
            "appearance" => f.edit(first, |props| props.background = Some([255, 0, 0, 255])),
            "content" => {
                let props = f.stage.node(first).unwrap().props.clone();
                f.stage
                    .update(
                        first,
                        NodeKind::Text(reel_core::text::Text::parse("new content").unwrap()),
                        props,
                    )
                    .unwrap();
            }
            "reorder" => {
                let node = f.stage.node(list).unwrap();
                let NodeKind::VirtualList(mut view) = node.kind.clone() else {
                    unreachable!()
                };
                let props = node.props.clone();
                let mut rows = view.rows.to_vec();
                let first_id = rows[0].node;
                rows[0].node = rows[1].node;
                rows[1].node = first_id;
                view.rows = rows.into();
                f.stage
                    .update(list, NodeKind::VirtualList(view), props)
                    .unwrap();
            }
            "far" => {
                let node = f.stage.node(f.root).unwrap();
                let mut kind = node.kind.clone();
                if let NodeKind::Scroll { offset, .. } = &mut kind {
                    offset.y = 1000.;
                }
                f.stage.update(f.root, kind, node.props.clone()).unwrap();
            }
            "modal" => f.edit(f.root, |props| props.modal = true),
            "unknown-size" => f.edit(waiting, |props| props.height = None),
            "outside-image" => {
                let image = f.stage.node(waiting).unwrap().clone();
                let outside = f.stage.create(image.kind, image.props, vec![]).unwrap();
                f.stage.add(f.root, outside, 1).unwrap();
            }
            "outside" => {
                let extra = f
                    .stage
                    .create(NodeKind::Stack, NodeProps::default(), vec![])
                    .unwrap();
                f.stage.add(f.root, extra, 1).unwrap();
            }
            _ => {}
        }
        f.ui.set_stage(&f.stage);
        f.ui.sync(&mut f.bridge, f.widget).unwrap();
        assert!(f.ui.images_pending(), "{case}");
        assert_eq!(
            !f.ui.images_block_presentation(),
            matches!(
                case,
                "unchanged" | "width" | "appearance" | "content" | "reorder" | "outside"
            ),
            "{case}"
        );
        if case == "width" {
            let geometry = f.bridge.layout_geometry().unwrap();
            assert_eq!(
                geometry.get(f.layouts()[&first].0).unwrap().size.width,
                112.
            );
        }
    }
}

#[test]
fn hidden_candidate_retires_cache_demand_and_accepts_its_hidden_tree() {
    let (mut f, _, _, _) = fixture();
    let retained = f.record();
    f.accept(&retained);
    f.edit(f.root, |props| props.visible = false);
    let hidden = f.record();
    assert!(hidden.receipt.current);
    assert!(!f.ui.images_pending());
    f.accept(&hidden);
    assert!(
        hidden
            .geometry()
            .get(f.layouts()[&f.root].0)
            .unwrap()
            .is_stashed
    );
    assert!(click(&mut f, 20.).is_empty());
    assert!(!f.ui.images_waiting_presentation);
}

#[test]
fn virtual_image_receipts_cannot_release_new_projection_capture_gate() {
    let (mut f, list, first, waiting) = fixture();
    let retained = f.record();
    assert!(!retained.receipt.current);
    assert!(!f.ui.images_block_presentation());
    f.accept(&retained);
    assert!(f.ui.images_waiting_presentation);
    assert_eq!(
        click(&mut f, 20.),
        vec![first],
        "the accepted unchanged visible row retains its own action"
    );
    f.stage.remove(waiting).unwrap();
    let node = f.stage.node(list).unwrap();
    let props = node.props.clone();
    let NodeKind::VirtualList(mut view) = node.kind.clone() else {
        unreachable!()
    };
    view.rows = view
        .rows
        .iter()
        .filter(|r| r.node != waiting)
        .copied()
        .collect();
    f.stage
        .update(list, NodeKind::VirtualList(view), props)
        .unwrap();
    let older = f.record();
    assert!(older.receipt.current);
    f.edit(first, |props| props.background = Some([20, 40, 60, 255]));
    let newest = f.record();
    f.accept(&older);
    assert!(
        f.ui.images_waiting_presentation,
        "late old stamp cannot accept the newer candidate"
    );
    f.accept(&newest);
    assert!(!f.ui.images_waiting_presentation);
}

fn click(f: &mut Fixture, y: f64) -> Vec<NodeId> {
    let mut clicked = Vec::new();
    for down in [true, false] {
        let event = PointerButtonEvent {
            pointer: PointerInfo {
                pointer_id: Some(PointerId::PRIMARY),
                persistent_device_id: None,
                pointer_type: PointerType::Mouse,
            },
            state: PointerState {
                position: reel_ui::dpi::PhysicalPosition::new(20., y),
                ..Default::default()
            },
            button: Some(PointerButton::Primary),
        };
        let result =
            f.ui.handle_pointer(
                &mut f.bridge,
                if down {
                    PointerEvent::Down(event)
                } else {
                    PointerEvent::Up(event)
                },
            )
            .unwrap();
        clicked.extend(result.events.into_iter().filter_map(|e| {
            if let NodeEvent::Click(e) = e {
                Some(e.node)
            } else {
                None
            }
        }));
    }
    clicked
}
