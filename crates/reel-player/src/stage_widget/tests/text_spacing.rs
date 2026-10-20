use super::*;
use reel_ui::imaging::record::{Command, Draw};

fn fixture(kind: NodeKind) -> Fixture {
    let font = reel_ui::peniko::Blob::new(Arc::new(
        include_bytes!("../../../tests/fixtures/font-weight/400.ttf").to_vec(),
    ));
    let mut f = Fixture::new(Some(font));
    f.edit(f.root, |props| {
        *props = NodeProps {
            width: Some(640.),
            height: Some(420.),
            ..Default::default()
        };
    });
    f.stage
        .update(
            f.blue,
            kind,
            NodeProps {
                width: Some(300.),
                font: "Reel Weight Fixture".into(),
                font_size: 20.,
                ..Default::default()
            },
        )
        .unwrap();
    f
}

#[derive(Debug, PartialEq)]
struct Position {
    x: f32,
    y: f32,
}
fn letters(frame: &TestFrame) -> Vec<Position> {
    let scene = frame.scene();
    scene
        .commands()
        .iter()
        .filter_map(|command| match command {
            Command::Draw(id) => match scene.draw_op(*id) {
                Draw::GlyphRun(run) => Some(run),
                _ => None,
            },
            _ => None,
        })
        .flat_map(|run| {
            run.glyphs
                .iter()
                .filter(|glyph| glyph.id == 2)
                .map(|glyph| {
                    let point = run.transform * Point::new(glyph.x.into(), glyph.y.into());
                    Position {
                        x: point.x as f32,
                        y: point.y as f32,
                    }
                })
        })
        .collect()
}

#[test]
fn text_spacing_moves_real_glyphs_and_keeps_stable_widget_layouts() {
    for kind in [
        NodeKind::Text("A\u{00a0}A A".into()),
        NodeKind::Button {
            label: Some("A A A".into()),
            listening: false,
        },
        NodeKind::TextInput {
            value: "A A A".into(),
        },
        NodeKind::Toggle {
            label: Some("A A A".into()),
            value: false,
            listening: false,
            appearance: reel_ui::stage::ToggleAppearance::Checkbox,
        },
        NodeKind::Radio {
            label: Some("A A A".into()),
            selected: false,
            group: None,
            revision: 0,
        },
    ] {
        let mut f = fixture(kind.clone());
        let initial = f.record();
        f.accept(&initial);
        let baseline = letters(&initial);
        assert_eq!(baseline.len(), 3);
        let identity = f.layouts()[&f.blue].0;
        for (letter, word) in [(2., -3.), (-2., 3.), (0., 0.)] {
            let before = f.layouts()[&f.blue].1;
            f.edit(f.blue, |props| {
                props.letter_spacing = letter;
                props.word_spacing = word;
            });
            let changed = f.record();
            f.accept(&changed);
            let glyphs = letters(&changed);
            assert_eq!(glyphs.len(), 3);
            for i in 0..3 {
                // Two clusters and one word-space precede each successive A.
                let expected = i as f32 * (2. * letter + word) as f32;
                let delta = (glyphs[i].x - glyphs[0].x) - (baseline[i].x - baseline[0].x);
                assert!(
                    (delta - expected).abs() < 0.001,
                    "{kind:?}: {delta} != {expected}"
                );
            }
            let layout = f.layouts()[&f.blue];
            assert_eq!(layout.0, identity);
            assert!(layout.1 > before);
            for _ in 0..120 {
                let stable = f.record();
                f.accept(&stable);
            }
            assert_eq!(f.layouts()[&f.blue], layout, "stable spacing reused layout");
            assert_eq!(
                letters(&initial),
                baseline,
                "old accepted frame retained its glyphs"
            );
        }
    }
}

#[test]
fn text_spacing_negative_words_reflow_and_keep_explicit_newlines() {
    let mut f = fixture(NodeKind::Text("A A A\nA A A".into()));
    f.edit(f.blue, |props| props.width = Some(35.));
    let initial = f.record();
    f.accept(&initial);
    let line_count = |frame: &TestFrame| {
        letters(frame)
            .iter()
            .map(|glyph| glyph.y.to_bits())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    };
    assert_eq!(line_count(&initial), 4);
    f.edit(f.blue, |props| props.word_spacing = -4.);
    let compact = f.record();
    f.accept(&compact);
    assert_eq!(line_count(&compact), 2);
    let widget = f.layouts()[&f.blue].0;
    assert!(
        compact.geometry().get(widget).unwrap().size.height
            < initial.geometry().get(widget).unwrap().size.height
    );
}

#[test]
fn text_spacing_includes_the_last_cluster_in_natural_width() {
    let mut f = fixture(NodeKind::Text("AA".into()));
    f.edit(f.blue, |props| props.width = None);
    let initial = f.record();
    f.accept(&initial);
    let widget = f.layouts()[&f.blue].0;
    let old_width = initial.geometry().get(widget).unwrap().size.width;
    f.edit(f.blue, |props| props.letter_spacing = 2.);
    let spaced = f.record();
    f.accept(&spaced);
    let new_width = spaced.geometry().get(widget).unwrap().size.width;
    assert!((new_width - old_width - 4.).abs() < 0.001);
}

#[test]
fn text_spacing_rich_size_ruby_and_shrink_use_logical_spacing() {
    let mut f = fixture(NodeKind::Text(
        reel_core::text::Text::parse("A[size=40]A[/size]A [ruby=AA]AA[/ruby]").unwrap(),
    ));
    let initial = f.record();
    f.accept(&initial);
    let baseline = letters(&initial);
    f.edit(f.blue, |props| props.letter_spacing = 3.);
    let spaced = f.record();
    f.accept(&spaced);
    let glyphs = letters(&spaced);
    // The ruby note is a separate baseline; its two letters inherit 3px, despite half-size.
    let rows = |glyphs: &[Position]| {
        let mut rows = BTreeMap::<u32, Vec<f32>>::new();
        for glyph in glyphs {
            rows.entry(glyph.y.to_bits()).or_default().push(glyph.x);
        }
        rows
    };
    let old_rows = rows(&baseline);
    let new_rows = rows(&glyphs);
    let (_, old_base) = old_rows.iter().find(|(_, row)| row.len() == 5).unwrap();
    let (_, new_base) = new_rows.iter().find(|(_, row)| row.len() == 5).unwrap();
    for i in 0..3 {
        assert!(
            ((new_base[i] - new_base[0]) - (old_base[i] - old_base[0]) - i as f32 * 3.).abs()
                < 0.001
        );
    }
    let (_, old_note) = old_rows.iter().find(|(_, row)| row.len() == 2).unwrap();
    let (_, new_note) = new_rows.iter().find(|(_, row)| row.len() == 2).unwrap();
    assert!(((new_note[1] - new_note[0]) - (old_note[1] - old_note[0]) - 3.).abs() < 0.001);
    f.edit(f.blue, |props| {
        props.width = Some(40.);
        props.text_fit = reel_ui::core::TextFit::Shrink { min_font_size: 0. };
    });
    let shrunk = f.record();
    f.accept(&shrunk);
    let small = letters(&shrunk);
    let small_rows = rows(&small);
    let (_, small_note) = small_rows.iter().find(|(_, row)| row.len() == 2).unwrap();
    assert!(small_note[1] - small_note[0] < new_note[1] - new_note[0]);
}

#[test]
fn text_spacing_negative_words_update_input_hit_and_caret() {
    use reel_ui::core::{
        Ime, PointerButton, PointerButtonEvent, PointerEvent, PointerId, PointerInfo, PointerState,
        PointerType, TextEvent,
    };
    use reel_ui::dpi::PhysicalPosition;
    use reel_ui::widgets::TextInputEvent;

    let mut f = fixture(NodeKind::TextInput {
        value: "A A A".into(),
    });
    let initial = f.record();
    f.accept(&initial);
    f.edit(f.blue, |props| props.word_spacing = -4.);
    let spaced = f.record();
    f.accept(&spaced);
    let positions = letters(&spaced);
    let scene = f.bridge.root().get_widget(f.layouts()[&f.blue].0).unwrap();
    let scene = scene.downcast::<SceneWidget>().unwrap();
    let editor = scene.editor.as_ref().unwrap().id();
    let geometry = spaced.geometry().get(editor).unwrap();
    let point = Point::new(
        f64::from(positions[1].x + 1.),
        (geometry.window_transform * Point::new(0., geometry.size.height * 0.5)).y,
    );
    let event = PointerButtonEvent {
        pointer: PointerInfo {
            pointer_id: Some(PointerId::PRIMARY),
            persistent_device_id: None,
            pointer_type: PointerType::Mouse,
        },
        state: PointerState {
            position: PhysicalPosition::new(point.x, point.y),
            ..Default::default()
        },
        button: Some(PointerButton::Primary),
    };
    f.ui.handle_pointer(&mut f.bridge, PointerEvent::Down(event.clone()))
        .unwrap();
    f.ui.handle_pointer(&mut f.bridge, PointerEvent::Up(event))
        .unwrap();
    let focused = f.record();
    f.accept(&focused);
    assert_eq!(f.bridge.root().focused_widget(), Some(editor));
    let outcome =
        f.ui.handle_text(&mut f.bridge, TextEvent::Ime(Ime::Commit("A".into())))
            .unwrap();
    assert!(
        outcome.events.iter().any(|event| matches!(
            event,
            NodeEvent::Text(NodeText { event: TextInputEvent::Changed { value, .. }, .. })
                if value.as_ref() == "A AA A"
        )),
        "the real pointer placed the insertion caret before the second A: {:?}",
        outcome.events
    );
}
