use super::*;
use reel_ui::imaging::record::{Command, Draw};

fn fonts() -> [reel_ui::peniko::Blob<u8>; 3] {
    [
        include_bytes!("../../../tests/fixtures/font-weight/400.ttf").as_slice(),
        include_bytes!("../../../tests/fixtures/font-weight/600.ttf").as_slice(),
        include_bytes!("../../../tests/fixtures/font-weight/700.ttf").as_slice(),
    ]
    .map(|bytes| reel_ui::peniko::Blob::new(Arc::new(bytes.to_vec())))
}
fn selected_fonts(frame: &TestFrame) -> std::collections::BTreeSet<u64> {
    let scene = frame.scene().unwrap();
    scene
        .commands()
        .iter()
        .filter_map(|command| match command {
            Command::Draw(id) => match scene.draw_op(*id) {
                Draw::GlyphRun(run) => {
                    assert!(run.glyphs.iter().all(|glyph| glyph.id != 0));
                    Some(run.font.data.id())
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn font_weight_selects_faces_for_labels_and_input_without_rebuilding_widgets() {
    let fonts = fonts();
    for kind in [
        NodeKind::Text("AAA".into()),
        NodeKind::Button {
            label: Some("AAA".into()),
            listening: false,
            focus_on_pointer: true,
            focusable: true,
        },
        NodeKind::TextInput {
            multiline: false,
            value: "AAA".into(),
            options: Default::default(),
        },
        NodeKind::Toggle {
            label: Some("AAA".into()),
            value: false,
            listening: false,
            appearance: reel_ui::stage::ToggleAppearance::Checkbox,
        },
        NodeKind::Radio {
            label: Some("AAA".into()),
            selected: false,
            group: None,
            revision: 0,
        },
    ] {
        let mut f = Fixture::new(None);
        f.edit(f.root, |p| {
            p.rotation = 0.;
            p.anchor_x = 0.;
            p.anchor_y = 0.;
            p.scale_x = 1.;
            p.x = 0.;
            p.y = 0.;
            p.width = Some(640.);
            p.height = Some(420.);
        });
        f.stage
            .update(
                f.blue,
                kind.clone(),
                NodeProps {
                    width: Some(300.),
                    height: Some(80.),
                    font: "Reel Weight Fixture".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        f.bridge.set_default_fonts(&fonts);
        let initial = f.record();
        f.accept(&initial);
        assert_eq!(selected_fonts(&initial), [fonts[0].id()].into(), "{kind:?}");
        let initial_layout = f.layouts()[&f.blue];
        for (weight, font) in [(600., &fonts[1]), (700., &fonts[2]), (400., &fonts[0])] {
            let before = f.layouts()[&f.blue];
            f.edit(f.blue, |props| props.font_weight = weight);
            let changed = f.record();
            f.accept(&changed);
            assert_eq!(
                selected_fonts(&changed),
                [font.id()].into(),
                "{kind:?} weight {weight}"
            );
            let after = f.layouts()[&f.blue];
            assert_eq!(after.0, initial_layout.0);
            assert!(after.1 > before.1);
            let stable = f.record();
            f.accept(&stable);
            assert_eq!(f.layouts()[&f.blue], after);
            assert_eq!(selected_fonts(&initial), [fonts[0].id()].into());
        }
    }
}

#[test]
fn font_weight_keeps_rich_bold_local_to_its_text_range() {
    let fonts = fonts();
    let mut f = Fixture::new(None);
    f.edit(f.root, |p| {
        p.rotation = 0.;
        p.anchor_x = 0.;
        p.anchor_y = 0.;
        p.scale_x = 1.;
        p.width = Some(640.);
        p.height = Some(420.);
    });
    let text = reel_core::text::Text::parse("A[b]A[/b]A").unwrap();
    f.stage
        .update(
            f.blue,
            NodeKind::Text(text),
            NodeProps {
                width: Some(300.),
                height: Some(80.),
                font: "Reel Weight Fixture".into(),
                font_weight: 600.,
                ..Default::default()
            },
        )
        .unwrap();
    f.bridge.set_default_fonts(&fonts);
    let frame = f.record();
    assert_eq!(
        selected_fonts(&frame),
        [fonts[1].id(), fonts[2].id()].into()
    );
}
#[test]
fn font_weight_600_uses_available_regular_face_when_family_has_no_semibold() {
    let fonts = fonts();
    let mut f = Fixture::new(None);
    f.edit(f.root, |p| {
        p.rotation = 0.;
        p.anchor_x = 0.;
        p.anchor_y = 0.;
        p.scale_x = 1.;
        p.width = Some(640.);
        p.height = Some(420.);
    });
    f.stage
        .update(
            f.blue,
            NodeKind::Text("AAA".into()),
            NodeProps {
                width: Some(300.),
                height: Some(80.),
                font: "Reel Weight Fixture".into(),
                font_weight: 600.,
                ..Default::default()
            },
        )
        .unwrap();
    f.bridge.set_default_fonts(&fonts[..1]);
    let frame = f.record();
    assert_eq!(selected_fonts(&frame), [fonts[0].id()].into());
    let scene = frame.scene().unwrap();
    for command in scene.commands() {
        if let Command::Draw(id) = command
            && let Draw::GlyphRun(run) = scene.draw_op(*id)
        {
            assert_eq!(run.font_embolden, Vec2::ZERO);
        }
    }
}
