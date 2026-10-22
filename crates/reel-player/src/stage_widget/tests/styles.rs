use super::*;
use reel_ui::imaging::record::{Command, Draw, Geometry};
use reel_ui::kurbo::Shape as _;

fn draws(source: &RecordedSource) -> impl Iterator<Item = &Draw> {
    let scene = source.scene();
    scene.commands().iter().filter_map(|command| match command {
        Command::Draw(id) => Some(scene.draw_op(*id)),
        _ => None,
    })
}

#[test]
fn box_styles_reuse_geometry_and_keep_inside_borders_bounded() {
    let mut fixture = Fixture::new(None);
    let first = fixture.record();
    fixture.accept(&first);
    let layouts = fixture.layouts();
    let widget = layouts[&fixture.red].0;
    let geometry = first.geometry().get(widget).unwrap().clone();
    drop(first);

    for width in [2., 1000., 0.] {
        fixture.edit(fixture.red, |props| {
            props.corner_radius = 8.;
            props.border_width = width;
            props.border_color = [0, 255, 0, 255];
        });
        let frame = fixture.record();
        fixture.accept(&frame);
        assert_eq!(fixture.layouts(), layouts, "decoration must not relayout");
        let current = frame.geometry().get(widget).unwrap();
        assert_eq!(current.size, geometry.size);
        assert_eq!(current.window_transform, geometry.window_transform);
        let border = draws(&frame).find_map(|draw| match draw {
            Draw::Fill {
                fill_rule: reel_ui::peniko::Fill::EvenOdd,
                shape: Geometry::Path(path),
                ..
            } => Some(path),
            _ => None,
        });
        if width == 0. {
            assert!(border.is_none());
            assert!(draws(&frame).any(|draw| matches!(draw,
                Draw::Fill { shape: Geometry::RoundedRect(rect), .. }
                    if rect.radii().top_left == 8.
            )));
        } else {
            let path = border.unwrap();
            let bounds = path.bounding_box();
            for (actual, expected) in [bounds.x0, bounds.y0, bounds.x1, bounds.y1]
                .into_iter()
                .zip([0., 0., 60., 30.])
            {
                assert!((actual - expected).abs() < 1e-9);
            }
            assert_eq!(path.winding(Point::new(30., 1.)).abs() % 2, 1);
            assert_eq!(path.winding(Point::new(0.1, 0.1)).abs() % 2, 0);
            assert_eq!(
                path.winding(Point::new(30., 15.)).abs() % 2,
                i32::from(width >= 15.)
            );
        }
    }
    let idle = fixture.record();
    fixture.accept(&idle);
    assert_eq!(fixture.layouts(), layouts);
}

#[test]
#[ignore = "requires REEL_SESSION_FONT with Latin glyphs"]
fn editor_styles_reach_initial_and_retained_widgets() {
    let font = reel_ui::peniko::Blob::from(
        std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap(),
    );
    let mut fixture = Fixture::new(Some(font));
    fixture.edit(fixture.root, |props| {
        *props = NodeProps {
            width: Some(600.),
            height: Some(400.),
            ..Default::default()
        };
    });
    fixture.edit(fixture.red, |props| props.visible = false);
    fixture
        .stage
        .update(
            fixture.blue,
            NodeKind::TextInput {
                value: "First line of notes contains enough words to wrap.\nSecond.".into(),
                multiline: true,
                options: Default::default(),
            },
            NodeProps {
                width: Some(120.),
                text_wrap: false,
                text_align: reel_ui::TextAlign::Center,
                line_height: 1.5,
                ..Default::default()
            },
        )
        .unwrap();
    let first = fixture.record();
    fixture.accept(&first);
    let widget = fixture.layouts()[&fixture.blue].0;
    let editor_id = || {
        fixture
            .bridge
            .root()
            .get_widget(widget)
            .unwrap()
            .downcast::<SceneWidget>()
            .unwrap()
            .editor
            .as_ref()
            .unwrap()
            .id()
    };
    let editor = editor_id();
    let first_height = first.geometry().get(widget).unwrap().size.height;
    let line_positions = |source: &RecordedSource| {
        draws(source)
            .filter_map(|draw| match draw {
                Draw::GlyphRun(run) => Some(run.glyphs.iter().map(|glyph| glyph.y.to_bits())),
                _ => None,
            })
            .flatten()
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(line_positions(&first).len(), 2);
    drop(first);

    fixture.edit(fixture.blue, |props| props.text_wrap = true);
    let wrapped = fixture.record();
    fixture.accept(&wrapped);
    assert!(line_positions(&wrapped).len() > 2);
    assert!(wrapped.geometry().get(widget).unwrap().size.height > first_height);
    drop(wrapped);

    fixture.edit(fixture.blue, |props| {
        props.text_wrap = false;
        props.line_height = 2.;
        props.text_align = reel_ui::TextAlign::Right;
    });
    let changed = fixture.record();
    fixture.accept(&changed);
    assert_eq!(line_positions(&changed).len(), 2);
    assert!(changed.geometry().get(widget).unwrap().size.height > first_height);
    assert_eq!(
        fixture
            .bridge
            .root()
            .get_widget(widget)
            .unwrap()
            .downcast::<SceneWidget>()
            .unwrap()
            .editor
            .as_ref()
            .unwrap()
            .id(),
        editor
    );
    drop(changed);
    let layouts = fixture.layouts();
    let idle = fixture.record();
    fixture.accept(&idle);
    assert_eq!(fixture.layouts(), layouts);
}
