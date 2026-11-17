//! Virtual row layout, navigation and accepted geometry.
use super::*;

#[test]
#[ignore = "requires REEL_SESSION_FONT for actual rich-text shaping"]
fn virtual_list_rich_text_layout_cost_depends_on_mounted_rows() {
    let bytes = std::fs::read(std::env::var_os("REEL_SESSION_FONT").unwrap()).unwrap();
    let mut counts = Vec::new();
    for total in [1000, 10_000, 100_000] {
        let (mut f, list, row) = virtual_fixture_with_font(total, Some(bytes.clone().into()));
        let rich = reel_core::text::Text::parse(
            "[b]A measured row[/b] with words that wrap across the available width. ".repeat(3),
        )
        .unwrap();
        f.stage
            .update(
                row,
                NodeKind::Text(rich),
                NodeProps {
                    font_size: 20.,
                    ..Default::default()
                },
            )
            .unwrap();
        let started = std::time::Instant::now();
        let first = f.record();
        f.accept(&first);
        let accepted = started.elapsed();
        let initial = f.layouts();
        let first_height =
            f.ui.take_events()
                .into_iter()
                .find_map(|e| match e {
                    NodeEvent::VirtualList(e) => Some(e.measurements[0].height),
                    _ => None,
                })
                .unwrap();
        assert!(first_height > 40.);
        counts.push(initial[&row].1);
        let props = f.stage.node(f.root).unwrap().props.clone();
        f.stage
            .update(
                f.root,
                NodeKind::Scroll {
                    overscroll: Default::default(),
                    axis: ScrollAxis::Vertical,
                    follow_end: false,
                    snap_y: None,
                    align: reel_ui::widgets::ScrollAlign::Start,
                    offset: Vec2::new(0., 10_000.),
                },
                props,
            )
            .unwrap();
        let moved = f.record();
        f.accept(&moved);
        assert_eq!(f.layouts(), initial, "scrolling reuses shaped text");
        f.ui.take_events();
        f.edit(f.root, |p| p.width = Some(100.));
        let narrow = f.record();
        f.accept(&narrow);
        let narrowed =
            f.ui.take_events()
                .into_iter()
                .find_map(|e| match e {
                    NodeEvent::VirtualList(e) if e.node == list => Some(e.measurements[0].height),
                    _ => None,
                })
                .unwrap();
        assert!(narrowed > first_height);
        assert_eq!(f.layouts()[&row].1, initial[&row].1 + 1);
        eprintln!(
            "virtual-rich-layout total={total} widgets={} first_row_layouts={} accepted_frame_us={} initial_height={first_height} narrow_height={narrowed}",
            initial.len(),
            initial[&row].1,
            accepted.as_micros()
        );
    }
    assert!(counts.iter().all(|c| *c == counts[0]));
}

#[test]
fn virtual_list_reports_only_mounted_rows_and_keeps_layouts_while_scrolling() {
    for total in [1_000, 10_000, 100_000] {
        let (mut f, list, row) = virtual_fixture(total);
        let initial = f.record();
        f.accept(&initial);
        let feedback =
            f.ui.take_events()
                .into_iter()
                .find_map(|event| match event {
                    NodeEvent::VirtualList(feedback) => Some(feedback),
                    _ => None,
                })
                .unwrap();
        assert_eq!(feedback.node, list);
        assert_eq!(feedback.width, 192.);
        assert_eq!(feedback.viewport, Size::new(200., 100.));
        assert_eq!(feedback.visible_start, -4.);
        assert_eq!(
            feedback.measurements.as_ref(),
            &[VirtualRowMeasurement {
                node: row,
                height: 40.
            }]
        );
        assert_eq!(
            f.layouts().len(),
            3,
            "total rows do not create placeholder widgets"
        );
        let before = f.layouts();
        let props = f.stage.node(f.root).unwrap().props.clone();
        f.stage
            .update(
                f.root,
                NodeKind::Scroll {
                    overscroll: Default::default(),
                    axis: ScrollAxis::Vertical,
                    follow_end: false,
                    snap_y: None,
                    align: reel_ui::widgets::ScrollAlign::Start,
                    offset: Vec2::new(0., 20_000.),
                },
                props,
            )
            .unwrap();
        let moved = f.record();
        f.accept(&moved);
        assert_eq!(
            f.layouts(),
            before,
            "a distant jump does not measure intermediate rows"
        );
        let feedback =
            f.ui.take_events()
                .into_iter()
                .find_map(|event| match event {
                    NodeEvent::VirtualList(feedback) => Some(feedback),
                    _ => None,
                })
                .unwrap();
        assert_eq!(feedback.visible_start, 19_996.);
        assert_eq!(feedback.measurements.len(), 1);
        let idle = f.record();
        f.accept(&idle);
        assert!(
            !f.ui
                .take_events()
                .iter()
                .any(|event| matches!(event, NodeEvent::VirtualList(_)))
        );
        assert_eq!(f.layouts(), before);
    }
}

#[test]
fn virtual_list_navigation_waits_for_accepted_rows_and_cancels_old_confirmation() {
    use reel_ui::input::{ActionPhase as P, NavigationAction as A, NavigationEvent};
    use reel_ui::stage::VirtualRow;
    for tab in [false, true] {
        let (mut f, list, first) = virtual_fixture(100);
        let nav = |action, phase| {
            TextEvent::Navigation(NavigationEvent {
                action,
                phase,
                value: 1.,
            })
        };
        let advance = || {
            if tab {
                TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Tab))
            } else {
                nav(A::Down, P::Pressed)
            }
        };
        let initial = f.record();
        f.accept(&initial);
        f.ui.handle_text(&mut f.bridge, advance()).unwrap();
        assert_eq!(
            f.bridge.root().focused_widget(),
            Some(f.layouts()[&first].0)
        );
        let next = f.ui.handle_text(&mut f.bridge, advance()).unwrap();
        assert!(next.events.iter().any(|event| matches!(event,
            NodeEvent::VirtualList(feedback) if feedback.navigation == Some(VirtualListNavigation { request: 1, ready: false, index: 1, resolved_index: 1, forward: true })
        )));
        assert!(
            next.events
                .iter()
                .filter_map(|event| match event {
                    NodeEvent::VirtualList(feedback) => Some(feedback),
                    _ => None,
                })
                .all(|feedback| feedback.retained.contains(&first))
        );
        f.ui.handle_text(&mut f.bridge, nav(A::Confirm, P::Pressed))
            .unwrap();
        let second = f
            .stage
            .create(
                NodeKind::Button {
                    label: None,
                    listening: true,
                    focus_on_pointer: true,
                    focusable: true,
                },
                NodeProps {
                    height: Some(40.),
                    ..Default::default()
                },
                vec![],
            )
            .unwrap();
        f.stage.add(list, second, 1).unwrap();
        let node = f.stage.node(list).unwrap();
        let NodeKind::VirtualList(mut view) = node.kind.clone() else {
            panic!()
        };
        let props = node.props.clone();
        view.revision += 1;
        view.rows = Arc::from([
            VirtualRow {
                node: first,
                index: 0,
                offset: 0.,
            },
            VirtualRow {
                node: second,
                index: 1,
                offset: 42.,
            },
        ]);
        view.navigation = Some(VirtualListNavigation {
            request: 1,
            ready: true,
            index: 1,
            resolved_index: 1,
            forward: true,
        });
        f.stage
            .update(list, NodeKind::VirtualList(view), props)
            .unwrap();
        let pending = f.record();
        assert!(
            f.ui.take_events()
                .iter()
                .filter_map(|event| match event {
                    NodeEvent::VirtualList(feedback) => Some(feedback),
                    _ => None,
                })
                .all(|feedback| feedback.navigation.is_some()),
            "recording does not complete navigation"
        );
        assert_eq!(
            f.bridge.root().focused_widget(),
            Some(f.layouts()[&first].0)
        );
        f.accept(&pending);
        assert_eq!(
            f.bridge.root().focused_widget(),
            Some(f.layouts()[&second].0)
        );
        assert!(
            f.ui.take_events().iter().any(|event| matches!(event,
                NodeEvent::VirtualList(feedback) if feedback.navigation.is_none()
            )),
            "accepted focus completes the engine request"
        );
        let release =
            f.ui.handle_text(&mut f.bridge, nav(A::Confirm, P::Released))
                .unwrap();
        assert!(
            !release
                .events
                .iter()
                .any(|event| matches!(event, NodeEvent::Click(_)))
        );
        f.ui.handle_text(&mut f.bridge, nav(A::Confirm, P::Pressed))
            .unwrap();
        let release =
            f.ui.handle_text(&mut f.bridge, nav(A::Confirm, P::Released))
                .unwrap();
        assert!(
            release
                .events
                .iter()
                .any(|event| matches!(event, NodeEvent::Click(click) if click.node == second))
        );
    }
}

#[test]
fn virtual_list_measures_changed_rows_and_coalesces_latest_viewport() {
    let (mut f, list, row) = virtual_fixture(10_000);
    let initial = f.record();
    f.accept(&initial);
    f.ui.take_events();
    f.edit(row, |props| props.height = Some(85.));
    f.edit(f.root, |props| props.width = Some(140.));
    let resized = f.record();
    f.accept(&resized);
    for offset in [50., 100., 200.] {
        let props = f.stage.node(f.root).unwrap().props.clone();
        f.stage
            .update(
                f.root,
                NodeKind::Scroll {
                    overscroll: Default::default(),
                    axis: ScrollAxis::Vertical,
                    follow_end: false,
                    snap_y: None,
                    align: reel_ui::widgets::ScrollAlign::Start,
                    offset: Vec2::new(0., offset),
                },
                props,
            )
            .unwrap();
        let frame = f.record();
        f.accept(&frame);
    }
    let feedback: Vec<_> =
        f.ui.take_events()
            .into_iter()
            .filter_map(|event| match event {
                NodeEvent::VirtualList(feedback) => Some(feedback),
                _ => None,
            })
            .collect();
    assert_eq!(
        feedback.len(),
        1,
        "unconsumed demand has one latest value per list"
    );
    assert_eq!(feedback[0].node, list);
    assert_eq!(feedback[0].width, 132.);
    assert_eq!(feedback[0].offset.y, 200.);
    assert_eq!(
        feedback[0].measurements.as_ref(),
        &[VirtualRowMeasurement {
            node: row,
            height: 85.
        }]
    );
}

#[test]
fn virtual_list_navigation_finds_controls_beyond_initial_nonfocusable_rows() {
    use reel_ui::stage::VirtualRow;
    for tab in [false, true] {
        let (mut f, list, first) = virtual_fixture(100);
        let props = f.stage.node(first).unwrap().props.clone();
        f.stage.update(first, NodeKind::Stack, props).unwrap();
        let initial = f.record();
        f.accept(&initial);
        f.ui.take_events();
        let event = if tab {
            TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Tab))
        } else {
            TextEvent::Navigation(reel_ui::input::NavigationEvent {
                action: reel_ui::input::NavigationAction::Down,
                phase: reel_ui::input::ActionPhase::Pressed,
                value: 1.,
            })
        };
        let request = f.ui.handle_text(&mut f.bridge, event).unwrap();
        assert!(request.events.iter().any(|event| matches!(event,
            NodeEvent::VirtualList(feedback) if feedback.navigation == Some(VirtualListNavigation { request: 1, ready: false, index: 1, resolved_index: 1, forward: true })
        )));
        for index in [1, 2] {
            let kind = if index == 1 {
                NodeKind::Stack
            } else {
                NodeKind::Button {
                    label: None,
                    listening: true,
                    focus_on_pointer: true,
                    focusable: true,
                }
            };
            let child = f
                .stage
                .create(
                    kind,
                    NodeProps {
                        height: Some(40.),
                        ..Default::default()
                    },
                    vec![],
                )
                .unwrap();
            f.stage.add(list, child, index).unwrap();
            let node = f.stage.node(list).unwrap();
            let NodeKind::VirtualList(mut view) = node.kind.clone() else {
                panic!()
            };
            let props = node.props.clone();
            let mut rows = view.rows.to_vec();
            rows.push(VirtualRow {
                node: child,
                index,
                offset: index as f64 * 42.,
            });
            view.rows = rows.into();
            view.revision += 1;
            view.navigation = Some(VirtualListNavigation {
                request: index as u64,
                ready: true,
                resolved_index: index,
                index,
                forward: true,
            });
            f.stage
                .update(list, NodeKind::VirtualList(view), props)
                .unwrap();
            let candidate = f.record();
            assert!(f.bridge.root().focused_widget().is_none());
            f.accept(&candidate);
            let feedback = f.ui.take_events();
            if index == 1 {
                assert!(f.bridge.root().focused_widget().is_none());
                assert!(feedback.iter().any(|event| matches!(event,
                    NodeEvent::VirtualList(feedback) if feedback.navigation == Some(VirtualListNavigation { request: 2, ready: false, index: 2, resolved_index: 2, forward: true })
                )), "nonfocusable rows continue only after acceptance");
            } else {
                assert_eq!(
                    f.bridge.root().focused_widget(),
                    Some(f.layouts()[&child].0)
                );
            }
        }
    }
}
