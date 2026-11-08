use super::*;

#[test]
fn hidden_scroll_retains_pending_offsets_until_visible_layout() {
    for nested in [false, true] {
        check_hidden_scroll(nested);
    }
}

fn check_hidden_scroll(nested: bool) {
    let mut f = Fixture::new(None);
    let mut stage = Stage::default();
    let content = stage
        .create(
            NodeKind::Stack,
            NodeProps {
                width: Some(800.),
                height: Some(2000.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let scroll = stage
        .create(
            NodeKind::Scroll {
                overscroll: Default::default(),
                axis: ScrollAxis::Both,
                follow_end: false,
                align: reel_ui::widgets::ScrollAlign::Start,
                offset: Vec2::new(90., 500.),
            },
            NodeProps {
                width: Some(400.),
                height: Some(400.),
                ..Default::default()
            },
            vec![content],
        )
        .unwrap();
    let root = stage
        .create(
            NodeKind::Stack,
            NodeProps {
                visible: false,
                width: Some(400.),
                height: Some(400.),
                ..Default::default()
            },
            vec![scroll],
        )
        .unwrap();
    let mounted = if nested {
        stage
            .create(NodeKind::Stack, NodeProps::default(), vec![root])
            .unwrap()
    } else {
        root
    };
    stage.replace(mounted).unwrap();
    f.stage = stage;
    f.root = mounted;
    // Rebuilding a saved hidden subtree creates widgets with pending offsets
    // and no layout yet. It must not feed an empty range back to the VM.
    for _ in 0..2 {
        let frame = f.record();
        f.accept(&frame);
        assert!(
            !f.ui
                .take_events()
                .iter()
                .any(|event| matches!(event, NodeEvent::Scroll(_)))
        );
    }
    f.edit(root, |props| props.visible = true);
    let frame = f.record();
    f.accept(&frame);
    let offsets: Vec<_> =
        f.ui.take_events()
            .into_iter()
            .filter_map(|event| match event {
                NodeEvent::Scroll(event) => Some(event),
                _ => None,
            })
            .collect();
    assert_eq!(offsets.len(), 1);
    assert_eq!(offsets[0].requested, Vec2::new(90., 500.));
    assert_eq!(offsets[0].offset, Vec2::new(90., 500.));
    assert_eq!(offsets[0].viewport, Size::new(400., 400.));
    assert_eq!(offsets[0].content, Size::new(800., 2000.));

    // A later hidden script write waits for the new content range too.
    f.edit(root, |props| props.visible = false);
    f.edit(content, |props| props.height = Some(600.));
    let props = f.stage.node(scroll).unwrap().props.clone();
    f.stage
        .update(
            scroll,
            NodeKind::Scroll {
                overscroll: Default::default(),
                axis: ScrollAxis::Both,
                follow_end: false,
                align: reel_ui::widgets::ScrollAlign::Start,
                offset: Vec2::new(120., 900.),
            },
            props,
        )
        .unwrap();
    let frame = f.record();
    f.accept(&frame);
    assert!(
        !f.ui
            .take_events()
            .iter()
            .any(|event| matches!(event, NodeEvent::Scroll(_)))
    );
    f.edit(root, |props| props.visible = true);
    let frame = f.record();
    f.accept(&frame);
    let actual =
        f.ui.take_events()
            .into_iter()
            .find_map(|event| match event {
                NodeEvent::Scroll(event) => Some(event),
                _ => None,
            })
            .unwrap();
    assert_eq!(actual.requested, Vec2::new(120., 900.));
    assert_eq!(actual.offset, Vec2::new(120., 200.));
    assert_eq!(actual.content, Size::new(800., 600.));
    // A real zero scroll range still clamps after layout.
    f.edit(content, |props| {
        props.width = Some(100.);
        props.height = Some(100.);
    });
    let frame = f.record();
    f.accept(&frame);
    let actual =
        f.ui.take_events()
            .into_iter()
            .find_map(|event| match event {
                NodeEvent::Scroll(event) => Some(event),
                _ => None,
            })
            .unwrap();
    assert_eq!(actual.offset, Vec2::ZERO);
}
