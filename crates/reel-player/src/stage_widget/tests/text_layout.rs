//! Text shaping, wrapping and incremental reveal.
use super::*;

#[test]
#[ignore = "requires REEL_LATIN_FONT without CJK and REEL_CJK_FONT covering Latin and CJK"]
fn configured_fonts_shape_mixed_text_and_live_family_changes_preserve_widget_identity() {
    use reel_ui::{
        frame::{BuildLimits, FrameBuild, FrameDraw, FrameInfo, FrameOp, StepBudget},
        imaging::record::{Command, Draw},
        reel_text::fontique::{Collection, CollectionOptions},
        resources::ResourceSnapshot,
    };
    let pool = reel_core::budget::BudgetPool::new(
        reel_core::budget::BudgetCategory::CpuOutput,
        reel_core::budget::QueueLimits {
            max_requests: 64,
            max_bytes: 64 << 20,
        },
    );
    let assets = crate::assets::Assets::directory(Default::default());
    let (send, receive) = std::sync::mpsc::channel();
    assets.set_waker(std::sync::Arc::new(move || {
        let _ = send.send(());
    }));
    let font = |key| {
        let path = std::env::var_os(key).expect(key);
        let id = assets.request(path.to_str().unwrap(), crate::assets::AssetKind::Font);
        let lease = loop {
            if let Some(lease) = assets.poll(id).unwrap() {
                break lease;
            }
            receive
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("font read must wake the host");
        };
        let reel_ui::resources::ResourceView::Font(data) = lease.view() else {
            unreachable!()
        };
        (data.clone(), lease)
    };
    let (latin, latin_lease) = font("REEL_LATIN_FONT");
    let (cjk, cjk_lease) = font("REEL_CJK_FONT");
    let mut collection = Collection::new(CollectionOptions {
        system_fonts: false,
        ..Default::default()
    });
    let families = collection.register_fonts(cjk.clone(), None);
    let family = collection.family_name(families[0].0).unwrap().to_owned();
    let mut f = Fixture::new(None);
    f.stage
        .update(
            f.blue,
            NodeKind::Text("ABC中文".into()),
            NodeProps {
                width: Some(400.),
                height: Some(60.),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(f.bridge.set_default_fonts(&[latin.clone(), cjk.clone()]));
    let used_fonts = |source: &RecordedSource| {
        let scene = source.scene().unwrap();
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
            .collect::<std::collections::BTreeSet<_>>()
    };
    let mixed = f.record();
    f.accept(&mixed);
    assert_eq!(used_fonts(&mixed), [latin.id(), cjk.id()].into());
    let original = f.layouts();
    f.edit(f.blue, |props| props.font = family.clone().into());
    let selected = f.record();
    f.accept(&selected);
    assert_eq!(used_fonts(&selected), [cjk.id()].into());
    assert_eq!(f.layouts()[&f.blue].0, original[&f.blue].0);
    assert!(f.layouts()[&f.blue].1 > original[&f.blue].1);
    assert!(f.bridge.set_default_fonts(&[cjk.clone(), latin]));
    f.edit(f.blue, |props| props.font = "system-ui".into());
    let reordered = f.record();
    f.accept(&reordered);
    assert_eq!(used_fonts(&reordered), [cjk.id()].into());
    // Old frames retain their original font data and glyph selection.
    assert_eq!(used_fonts(&mixed).len(), 2);
    let mut build = FrameBuild::new(
        mixed.source,
        ResourceSnapshot::capture([Ok(latin_lease), Ok(cjk_lease)].into_iter(), &pool).unwrap(),
        FrameInfo {
            logical_size: [640., 420.],
            physical_size: [640, 420],
        },
        BuildLimits { max_bytes: 8 << 20 },
        &pool,
    )
    .unwrap();
    while !build
        .step(StepBudget {
            max_commands: 64,
            max_units: 8192,
            max_bytes: 64 << 10,
        })
        .unwrap()
        .complete
    {}
    let frame = build.finish().unwrap();
    let frame_fonts = frame
        .commands()
        .iter()
        .filter_map(|command| match &command.op {
            FrameOp::Draw(FrameDraw::GlyphRun(run)) => Some(run.font),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(frame_fonts.len(), 2);
    assert!(!f.bridge.set_default_fonts(&[]));
    eprintln!("mixed Latin/CJK fallback, live family selection and priority reorder verified");
}

#[test]
#[ignore = "requires REEL_SESSION_FONT with Latin glyphs for real wrapping geometry"]
fn text_wrap_changes_height_preserves_newlines_and_reuses_the_label() {
    use reel_ui::imaging::record::{Command, Draw};

    let font = reel_ui::peniko::Blob::from(
        std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap(),
    );
    let mut f = Fixture::new(Some(font));
    f.edit(f.root, |props| {
        *props = NodeProps {
            width: Some(600.),
            height: Some(400.),
            ..Default::default()
        };
    });
    let text: reel_core::text::Text =
        "A deliberately long authored sentence that spans several widths.\nSecond explicit line."
            .into();
    f.stage
        .update(
            f.blue,
            NodeKind::Text(text.clone()),
            NodeProps {
                width: Some(120.),
                font_size: 24.,
                text_wrap: false,
                ..Default::default()
            },
        )
        .unwrap();
    let glyphs = |source: &RecordedSource| {
        let scene = source.scene().unwrap();
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
            .flat_map(|run| run.glyphs.iter().copied())
            .collect::<Vec<_>>()
    };
    let label_identity = |f: &Fixture| {
        let scene = f.bridge.root().get_widget(f.layouts()[&f.blue].0).unwrap();
        let scene = scene.downcast::<SceneWidget>().unwrap();
        let id = scene.label.as_ref().unwrap().id();
        let label = f.bridge.root().get_widget(id).unwrap();
        (id, label.downcast::<Label>().unwrap().text().clone())
    };
    let unwrapped = f.record();
    f.accept(&unwrapped);
    let (label_id, plain) = label_identity(&f);
    let widget_id = f.layouts()[&f.blue].0;
    let height = |source: &RecordedSource| source.geometry().get(widget_id).unwrap().size.height;
    let original_height = height(&unwrapped);
    let original_glyphs = glyphs(&unwrapped);
    assert!(!original_glyphs.is_empty());
    assert!(original_glyphs.iter().all(|glyph| glyph.id != 0));
    assert_eq!(
        original_glyphs
            .iter()
            .map(|glyph| glyph.y.to_bits())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        2,
        "only the explicit newline breaks text"
    );
    assert!(
        original_glyphs.iter().any(|glyph| glyph.x > 120.),
        "overflow keeps the complete authored line"
    );
    drop(unwrapped);

    f.edit(f.blue, |props| props.text_wrap = true);
    let wrapped = f.record();
    f.accept(&wrapped);
    assert!(
        height(&wrapped) > original_height * 2.,
        "automatic wrapping changes natural height"
    );
    assert_eq!(label_identity(&f).0, label_id);
    assert!(Arc::ptr_eq(&label_identity(&f).1, &plain));
    drop(wrapped);

    f.edit(f.blue, |props| props.text_wrap = false);
    let restored = f.record();
    f.accept(&restored);
    assert_eq!(height(&restored), original_height);
    assert_eq!(glyphs(&restored), original_glyphs);
    drop(restored);
    f.edit(f.blue, |props| props.text_outline = 2.);
    let outlined = f.record();
    f.accept(&outlined);
    assert_eq!(
        height(&outlined),
        original_height,
        "outline changes ink bounds only"
    );
    drop(outlined);
    let layouts = f.layouts();
    f.edit(f.blue, |props| props.reveal = Some(0));
    let hidden = f.record();
    f.accept(&hidden);
    assert!(glyphs(&hidden).is_empty());
    assert_eq!(f.layouts(), layouts, "reveal must not relayout");
    drop(hidden);
    f.edit(f.blue, |props| {
        props.reveal = Some(text.len());
        props.text_outline_color = [255, 0, 255, 255];
    });
    let revealed = f.record();
    f.accept(&revealed);
    assert!(!glyphs(&revealed).is_empty());
    assert_eq!(f.layouts(), layouts);
    drop(revealed);
    f.edit(f.blue, |props| props.text_outline = 0.);
    let filled = f.record();
    f.accept(&filled);
    assert_eq!(glyphs(&filled), original_glyphs);
    assert_eq!(height(&filled), original_height);
    drop(filled);

    f.edit(f.blue, |props| {
        props.text_shadow_x = -1.;
        props.text_shadow_y = 2.;
    });
    let shadowed = f.record();
    f.accept(&shadowed);
    assert_eq!(height(&shadowed), original_height);
    assert_eq!(glyphs(&shadowed).len(), original_glyphs.len() * 2);
    assert_eq!(label_identity(&f).0, label_id);
    drop(shadowed);
    let layouts = f.layouts();
    f.edit(f.blue, |props| props.text_shadow_color = [255, 0, 0, 255]);
    let recolored = f.record();
    f.accept(&recolored);
    assert_eq!(f.layouts(), layouts, "shadow color reuses the paragraph");
    drop(recolored);
    f.edit(f.blue, |props| {
        props.text_shadow_x = 0.;
        props.text_shadow_y = 0.;
    });

    f.edit(f.blue, |props| {
        props.text_fit = reel_ui::core::TextFit::Shrink { min_font_size: 0. }
    });
    let fitted = f.record();
    f.accept(&fitted);
    let fitted_height = height(&fitted);
    let fitted_glyphs = glyphs(&fitted);
    drop(fitted);
    f.edit(f.blue, |props| props.text_wrap = true);
    let fitted_wrapped = f.record();
    f.accept(&fitted_wrapped);
    assert!(fitted_height < original_height);
    assert_eq!(fitted_height, height(&fitted_wrapped));
    assert_eq!(
        fitted_glyphs,
        glyphs(&fitted_wrapped),
        "shrink is independent of automatic wrapping"
    );
    assert_eq!(label_identity(&f).0, label_id);
    assert!(Arc::ptr_eq(&label_identity(&f).1, &plain));
}

#[test]
fn text_wrap_keeps_control_labels_on_their_existing_overflow_path() {
    let mut f = Fixture::new(None);
    f.stage
        .update(
            f.blue,
            NodeKind::Button {
                label: Some("A control label keeps its existing layout".into()),
                listening: true,
                focus_on_pointer: true,
                focusable: true,
            },
            NodeProps {
                width: Some(120.),
                ..Default::default()
            },
        )
        .unwrap();
    let original = f.record();
    f.accept(&original);
    let layouts = f.layouts();
    f.edit(f.blue, |props| props.text_wrap = false);
    let changed = f.record();
    f.accept(&changed);
    assert_eq!(f.layouts(), layouts);
}

#[test]
fn dialogue_reveal_keeps_text_identity_and_layout() {
    let mut f = Fixture::new(None);
    let first = f.record();
    f.accept(&first);
    let original = f.layouts();
    let NodeKind::Text(text) = &f.stage.node(f.blue).unwrap().kind else {
        unreachable!()
    };
    let text = text.clone();
    for end in 0..=text.len() {
        f.edit(f.blue, |props| props.reveal = Some(end));
        let frame = f.record();
        f.accept(&frame);
        assert_eq!(f.layouts(), original, "reveal must not cause layout");
        let NodeKind::Text(current) = &f.stage.node(f.blue).unwrap().kind else {
            unreachable!()
        };
        assert!(Arc::ptr_eq(text.plain(), current.plain()));
    }
}

#[test]
#[ignore = "requires REEL_SESSION_FONT for wrapped text measurement"]
fn authored_text_width_controls_scroll_extent_independently_of_parent_width() {
    let font = std::fs::read(std::env::var_os("REEL_SESSION_FONT").unwrap()).unwrap();
    let mut f = Fixture::new(Some(font.into()));
    f.edit(f.red, |p| p.visible = false);
    f.edit(f.blue, |p| {
        *p = NodeProps {
            width: Some(110.),
            font_size: 24.,
            ..Default::default()
        };
    });
    let props = f.stage.node(f.blue).unwrap().props.clone();
    f.stage
        .update(
            f.blue,
            NodeKind::Text(
                "A wrapped chat message stays completely visible. "
                    .repeat(10)
                    .into(),
            ),
            props,
        )
        .unwrap();
    let props = NodeProps {
        width: Some(200.),
        height: Some(90.),
        ..Default::default()
    };
    f.stage
        .update(
            f.root,
            NodeKind::Scroll {
                overscroll: Default::default(),
                axis: ScrollAxis::Vertical,
                follow_end: true,
                snap_y: None,
                align: reel_ui::widgets::ScrollAlign::End,
                offset: Vec2::ZERO,
            },
            props,
        )
        .unwrap();
    let mut measured = None;
    for capped in [false, true] {
        f.edit(f.blue, |p| {
            p.width = if capped { None } else { Some(110.) };
            p.max_width = capped.then_some(110.);
        });
        for width in [200., 500., 140.] {
            f.edit(f.root, |p| p.width = Some(width));
            let source = f.record();
            f.accept(&source);
            let id = f.layouts()[&f.root].0;
            let root = f
                .bridge
                .root()
                .get_widget(id)
                .unwrap()
                .downcast::<SceneWidget>()
                .unwrap();
            let height = root.scroll.content().height;
            assert!(height > 90., "the paragraph must overflow the viewport");
            assert_eq!(
                height,
                *measured.get_or_insert(height),
                "explicit text width determines every wrapped line"
            );
            assert_eq!(
                root.scroll.offset().y,
                height - 90.,
                "end following includes the complete last line"
            );
        }
    }
}

#[test]
fn reveal_fade_live_node_updates_keep_stage_label_and_layout_identity() {
    let label_identity = |f: &Fixture| {
        let scene = f.bridge.root().get_widget(f.layouts()[&f.blue].0).unwrap();
        scene
            .downcast::<SceneWidget>()
            .unwrap()
            .label
            .as_ref()
            .unwrap()
            .id()
    };
    let mut f = Fixture::new(None);
    let first = f.record();
    f.accept(&first);
    let before = f.layouts();
    let label_id = label_identity(&f);
    for amount in [0., 0.5, 1., 2.] {
        f.edit(f.blue, |p| {
            p.reveal_fade = Some(amount);
            p.reveal_fade_step = 0.25;
            p.reveal_fade_from = 0;
        });
        let frame = f.record();
        f.accept(&frame);
        assert_eq!(f.layouts(), before);
        assert_eq!(label_identity(&f), label_id);
    }
}
