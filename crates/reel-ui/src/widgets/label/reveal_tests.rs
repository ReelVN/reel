use super::*;
use crate::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    core::{DefaultProperties, Widget},
    dpi::PhysicalSize,
    peniko::Blob,
};
use std::sync::Arc;

#[test]
fn reveal_fade_uses_unicode_graphemes_and_preserves_opaque_prefix() {
    let text = "Aé e\u{301}Z";
    let starts: Vec<_> = text.grapheme_indices(true).map(|(i, _)| i).collect();
    let fade = crate::core::RevealFade {
        from: 1,
        progress: 0.75,
        step: 0.25,
    };
    assert_eq!(fade.alpha(&starts, 0..1), 1.);
    assert_eq!(fade.alpha(&starts, 1..3), 0.75);
    let combined = text.find('e').unwrap();
    assert_eq!(fade.alpha(&starts, combined..combined + 3), 0.25);
    assert_eq!(fade.alpha(&starts, text.len() - 1..text.len()), 0.);
    assert_eq!(fade.alpha(&starts, 1..text.len()), 0.);
}
#[test]
fn rich_text_and_fade_builder_order_share_graphemes_and_disabling_releases_index() {
    let fade = crate::core::RevealFade {
        from: 0,
        progress: 0.75,
        step: 0.25,
    };
    let source = Text::from("Aé e\u{301}Z");
    let first = Label::new("old")
        .with_reveal_fade(Some(fade))
        .with_rich_text(source.clone());
    let second = Label::new("old")
        .with_rich_text(source)
        .with_reveal_fade(Some(fade));
    assert_eq!(first.fade_graphemes, vec![0, 1, 3, 4, 7]);
    assert_eq!(first.fade_graphemes, second.fade_graphemes);
    let disabled = first.with_reveal_fade(None);
    assert_eq!(disabled.fade_graphemes.capacity(), 0);
}
#[test]
#[ignore = "requires REEL_SESSION_FONT; CPU glyph primitives only"]
fn reveal_fade_records_alpha_and_keeps_cached_layout_and_grapheme_index() {
    let font = Blob::from(
        std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap(),
    );
    let label = Label::new("ABCDE")
        .with_reveal_fade(Some(crate::core::RevealFade {
            from: 0,
            progress: 0.75,
            step: 0.25,
        }))
        .prepare();
    let id = label.id();
    let mut root = RenderRoot::new(
        label,
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(400, 100),
            scale_factor: 1.,
            default_font: Some(font),
        },
    );
    let (plan, _) = root.redraw();
    let mut opacity = Vec::new();
    for layer in &plan.layers {
        if let crate::app::VisualLayerKind::Scene(scene) = &layer.kind {
            for command in scene.commands() {
                if let crate::imaging::record::Command::Draw(id) = command
                    && let crate::imaging::record::Draw::GlyphRun(run) = scene.draw_op(*id)
                {
                    opacity.extend(std::iter::repeat_n(run.composite.alpha, run.glyphs.len()));
                }
            }
        }
    }
    assert_eq!(opacity, [0.75, 0.5, 0.25]);
    let before = {
        let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
        (
            label.cache_time,
            label.active_layout,
            label.fade_graphemes.as_ptr(),
            label.layouts[label.active_layout].layout.width(),
            label.layouts[label.active_layout].layout.height(),
        )
    };
    for progress in [0., 0.2, 0.5, 1., 2.] {
        root.edit_widget(id, |mut widget| {
            Label::set_reveal_fade(
                &mut widget.downcast::<Label>(),
                Some(crate::core::RevealFade {
                    from: 0,
                    progress,
                    step: 0.25,
                }),
            )
        });
        root.redraw();
        let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
        assert_eq!(
            (
                label.cache_time,
                label.active_layout,
                label.fade_graphemes.as_ptr(),
                label.layouts[label.active_layout].layout.width(),
                label.layouts[label.active_layout].layout.height()
            ),
            before
        );
    }
    root.edit_widget(id, |mut label| {
        Label::set_reveal_fade(&mut label.downcast::<Label>(), None)
    });
    let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
    assert_eq!(label.fade_graphemes.capacity(), 0);
}

#[test]
#[ignore = "requires REEL_SESSION_FONT; CPU shadow and shared glyph layout only"]
fn shadow_overrides_rich_color_and_reuses_faded_glyphs_and_layout() {
    let font = Blob::from(
        std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap(),
    );
    let shadow_color = crate::peniko::Color::from_rgb8(13, 29, 71);
    let label = Label::new("")
        .with_rich_text(Text::parse("[color=#ff0000][u][ruby=xy]AB[/ruby]CD[/u][/color]").unwrap())
        .with_shadow(Some(TextShadow {
            offset: crate::kurbo::Vec2::new(2., 3.),
            color: shadow_color,
        }))
        .with_outline(Some(TextOutline {
            width: 1.,
            color: crate::peniko::Color::BLACK,
        }))
        .with_reveal_fade(Some(crate::core::RevealFade {
            from: 0,
            progress: 0.75,
            step: 0.25,
        }))
        .prepare();
    let id = label.id();
    let mut root = RenderRoot::new(
        label,
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(400, 150),
            scale_factor: 1.,
            default_font: Some(font),
        },
    );
    let (plan, _) = root.redraw();
    let mut shadow = Vec::new();
    let mut foreground = Vec::new();
    for layer in &plan.layers {
        if let crate::app::VisualLayerKind::Scene(scene) = &layer.kind {
            for command in scene.commands() {
                if let crate::imaging::record::Command::Draw(draw) = command
                    && let crate::imaging::record::Draw::GlyphRun(run) = scene.draw_op(*draw)
                {
                    let key = (
                        run.glyphs.to_vec(),
                        run.font_size,
                        run.composite.alpha,
                        run.style.clone(),
                    );
                    if run.brush == crate::peniko::Brush::Solid(shadow_color) {
                        shadow.push((key, run.transform));
                    } else {
                        foreground.push((key, run.transform));
                    }
                }
            }
        }
    }
    assert!(!shadow.is_empty());
    assert_eq!(shadow.len(), foreground.len());
    for ((a, at), (b, bt)) in shadow.iter().zip(&foreground) {
        assert_eq!(
            a, b,
            "shadow must share glyphs, outline and fade with foreground"
        );
        assert_eq!(*at, Affine::translate((2., 3.)) * *bt);
    }
    let before = {
        let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
        (label.cache_time, label.active_layout, label.layouts.len())
    };
    root.edit_widget(id, |mut label| {
        Label::set_shadow(
            &mut label.downcast::<Label>(),
            Some(TextShadow {
                offset: crate::kurbo::Vec2::new(2., 3.),
                color: crate::peniko::Color::WHITE,
            }),
        )
    });
    root.redraw();
    let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
    assert_eq!(
        before,
        (label.cache_time, label.active_layout, label.layouts.len())
    );
}

#[test]
#[ignore = "requires REEL_SESSION_FONT; CPU rich glyph primitives and hit geometry only"]
fn reveal_fade_ruby_outline_decorations_and_link_visibility_share_alpha() {
    let font = Blob::from(
        std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap(),
    );
    let source =
        Text::parse("[link=entry][u][s][em][ruby=XY]AB[/ruby]CD[/em][/s][/u][/link]").unwrap();
    let label = Label::new("")
        .with_rich_text(source)
        .with_style(StyleProperty::FontSize(24.))
        .with_outline(Some(TextOutline {
            width: 1.,
            color: crate::peniko::Color::BLACK,
        }))
        .with_reveal_fade(Some(crate::core::RevealFade {
            from: 0,
            progress: 0.75,
            step: 0.25,
        }))
        .prepare();
    let id = label.id();
    let mut root = RenderRoot::new(
        label,
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(400, 150),
            scale_factor: 1.,
            default_font: Some(font),
        },
    );
    let (plan, _) = root.redraw();
    let mut ruby = 0;
    let mut outline = 0;
    let mut decorations = 0;
    for layer in &plan.layers {
        if let crate::app::VisualLayerKind::Scene(scene) = &layer.kind {
            for command in scene.commands() {
                if let crate::imaging::record::Command::Draw(draw) = command {
                    match scene.draw_op(*draw) {
                        crate::imaging::record::Draw::GlyphRun(run) => {
                            assert!(run.composite.alpha > 0. && run.composite.alpha <= 0.75);
                            if run.font_size < 20. {
                                assert_eq!(run.composite.alpha, 0.5);
                                ruby += run.glyphs.len();
                            }
                            if matches!(run.style, crate::peniko::Style::Stroke(_)) {
                                outline += run.glyphs.len();
                            }
                        }
                        crate::imaging::record::Draw::Fill { composite, .. } => {
                            assert!(composite.alpha > 0. && composite.alpha <= 0.75);
                            decorations += 1;
                        }
                        crate::imaging::record::Draw::Stroke { composite, .. } => {
                            assert!(composite.alpha > 0. && composite.alpha <= 0.75);
                            decorations += 1;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    assert!(ruby > 0 && outline > 0 && decorations > 0);
    assert!(
        !root
            .get_widget(id)
            .unwrap()
            .downcast::<Label>()
            .unwrap()
            .link_visible(0)
    );
    root.edit_widget(id, |mut widget| {
        Label::set_reveal_fade(
            &mut widget.downcast::<Label>(),
            Some(crate::core::RevealFade {
                from: 0,
                progress: 1.01,
                step: 0.25,
            }),
        )
    });
    root.redraw();
    assert!(
        root.get_widget(id)
            .unwrap()
            .downcast::<Label>()
            .unwrap()
            .link_visible(0)
    );
    root.edit_widget(id, |mut widget| {
        Label::set_visible_bytes(&mut widget.downcast::<Label>(), 0..3)
    });
    root.redraw();
    assert!(
        !root
            .get_widget(id)
            .unwrap()
            .downcast::<Label>()
            .unwrap()
            .link_visible(0)
    );
}

#[test]
#[ignore = "requires REEL_SESSION_FONT; CPU scene recording only"]
fn moving_long_text_refreshes_visible_glyphs_without_rebuilding_layout() {
    let font = Blob::from(
        std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap(),
    );
    let label = crate::core::NewWidget::new(Label::new("A line of history\n".repeat(2000)))
        .with_props(LineBreaking::WordWrap);
    let id = label.id();
    let mut root = RenderRoot::new(
        label,
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(840, 280),
            scale_factor: 1.,
            default_font: Some(font),
        },
    );
    root.redraw();
    let snapshot = |root: &RenderRoot| {
        let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
        let layout = &label.layouts[label.active_layout].layout;
        (
            label.cache_time,
            label.active_layout,
            layout.visible_lines(label.paint_viewport, Affine::IDENTITY, None),
        )
    };
    let before = snapshot(&root);
    assert_eq!(before.2.start, 0);
    root.edit_widget(id, |mut widget| {
        widget.set_transform(Affine::translate((0., -10_000.)))
    });
    let (plan, _) = root.redraw();
    let after = snapshot(&root);
    assert_eq!(
        (before.0, before.1),
        (after.0, after.1),
        "scrolling must reuse shaping"
    );
    assert!(
        after.2.start > before.2.end,
        "newly exposed lines must replace cached paint"
    );
    let glyphs: usize = plan
        .layers
        .iter()
        .map(|layer| match &layer.kind {
            crate::app::VisualLayerKind::Scene(scene) => scene
                .commands()
                .iter()
                .map(|command| match command {
                    crate::imaging::record::Command::Draw(id) => match scene.draw_op(*id) {
                        crate::imaging::record::Draw::GlyphRun(run) => run.glyphs.len(),
                        _ => 0,
                    },
                    _ => 0,
                })
                .sum(),
            _ => 0,
        })
        .sum();
    assert!(
        glyphs > 0 && glyphs < 1000,
        "frame should contain only visible history lines: {glyphs}"
    );
    root.edit_widget(id, |mut widget| {
        let label = widget.downcast::<Label>();
        let viewport = label.widget.paint_viewport.unwrap();
        assert!(
            !label.widget.set_paint_viewport(
                viewport + crate::kurbo::Vec2::new(20., 0.),
                Affine::IDENTITY,
            ),
            "horizontal motion with the same visible lines must preserve cached paint"
        );
        assert!(label.widget.set_paint_viewport(
            viewport + crate::kurbo::Vec2::new(100_000., 0.),
            Affine::IDENTITY,
        ));
        assert!(label.widget.paint_line_range.is_empty());
        assert!(label.widget.set_paint_viewport(viewport, Affine::IDENTITY));
        assert!(!label.widget.paint_line_range.is_empty());
    });
}

#[test]
#[ignore = "requires REEL_SESSION_FONT for shaping ruby and styled paragraphs"]
fn styled_ruby_layout_reveal_and_link_hit_geometry_share_one_cached_paragraph() {
    let font = Blob::from(
        std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap(),
    );
    let source = Text::parse("[link=entry][ruby=reading][b]Base[/b][/ruby][/link] [size=36]Large[/size] [em]ffi marked text[/em] across several lines").unwrap();
    let label = crate::core::NewWidget::new(
        Label::new("")
            .with_rich_text(source.clone())
            .with_style(StyleProperty::LineHeight(
                crate::reel_text::LineHeight::FontSizeRelative(2.),
            ))
            .with_visible_bytes(0..0),
    )
    .with_props(LineBreaking::WordWrap);
    let id = label.id();
    let mut root = RenderRoot::new(
        label,
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(210, 400),
            scale_factor: 1.,
            default_font: Some(font),
        },
    );
    root.redraw();
    let snapshot = |root: &RenderRoot| {
        let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
        let layout = &label.layouts[label.active_layout].layout;
        (
            label.cache_time,
            label.active_layout,
            layout.width(),
            layout.height(),
            layout
                .lines()
                .map(|l| (l.text_range(), l.metrics().baseline))
                .collect::<Vec<_>>(),
        )
    };
    let before = snapshot(&root);
    assert!(before.4.len() > 1, "mixed-size paragraph should wrap");
    let final_caret = root
        .get_widget(id)
        .unwrap()
        .downcast::<Label>()
        .unwrap()
        .text_caret()
        .unwrap();
    assert!(final_caret.p1.y > final_caret.p0.y);
    let (point, baseline) = {
        let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
        let layout = &label.layouts[label.active_layout].layout;
        let line = layout.get(0).unwrap();
        let p = line
            .items()
            .find_map(|i| match i {
                crate::reel_text::PositionedLayoutItem::InlineBox(p) => Some(p),
                _ => None,
            })
            .expect("ruby box");
        assert!(p.height > 24. && p.y >= 0. && p.y + p.height <= layout.height());
        let point = Point::new((p.x + p.width / 2.) as f64, (p.y + 2.) as f64);
        assert_eq!(
            label.link_at(point),
            None,
            "hidden text must not be clickable"
        );
        (point, line.metrics().baseline)
    };
    assert!(baseline > 24., "ruby reserves ascent");
    for end in [1, 3, 4, 10, source.len()] {
        root.edit_widget(id, |mut w| {
            Label::set_visible_bytes(&mut w.downcast::<Label>(), 0..end)
        });
        root.redraw();
        assert_eq!(
            snapshot(&root),
            before,
            "reveal must not reshape or change line breaks"
        );
        let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
        assert_eq!(label.link_at(point), (end >= 4).then_some(0));
        assert_eq!(
            label.text_caret(),
            Some(final_caret),
            "reveal preserves final line geometry"
        );
        assert_eq!(label.text_end(), Some(final_caret.p1));
    }
    root.edit_widget(id, |mut w| {
        Label::set_rich_text(
            &mut w.downcast::<Label>(),
            Text::parse("[ruby=only]base[/ruby]").unwrap(),
        )
    });
    root.redraw();
    let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
    let layout = &label.layouts[label.active_layout].layout;
    assert!(
        layout.width() > 0. && layout.height() > 0.,
        "a ruby-only paragraph has real geometry"
    );
    let caret = label.text_caret().unwrap();
    let last_line = layout.lines().last().unwrap();
    let last = last_line.metrics();
    assert_eq!(caret.p0.y, last.min_coord as f64);
    assert_eq!(caret.p1.y, last.max_coord as f64);

    let fitted = Text::parse("[link=entry][ruby=reading]Extraordinary[/ruby][/link] [size=36]possibilities[/size]\nSecond line").unwrap();
    root.edit_widget(id, |mut widget| {
        let mut label = widget.downcast::<Label>();
        Label::set_rich_text(&mut label, fitted.clone());
        Label::set_text_fit(&mut label, TextFit::Shrink { min_font_size: 0. });
        Label::set_visible_bytes(&mut label, 0..fitted.len());
    });
    root.redraw();
    let (fitted_layout, cache_len) = {
        let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
        let layout = &label.layouts[label.active_layout];
        assert!(layout.scale < 1.);
        assert!(layout.layout.width() <= 210.01, "{}", layout.layout.width());
        assert_eq!(
            layout.layout.len(),
            2,
            "only explicit newline breaks fitted text"
        );
        let ruby = layout
            .layout
            .get(0)
            .unwrap()
            .items()
            .find_map(|item| match item {
                crate::reel_text::PositionedLayoutItem::InlineBox(p) => Some(p),
                _ => None,
            })
            .unwrap();
        let point = Point::new((ruby.x + ruby.width / 2.) as f64, (ruby.y + 2.) as f64);
        assert_eq!(
            label.link_at(point),
            Some(0),
            "fitted ruby uses fitted link geometry"
        );
        (label.active_layout, label.layouts.len())
    };
    for end in [0, fitted.len()] {
        root.edit_widget(id, |mut widget| {
            let mut label = widget.downcast::<Label>();
            Label::set_visible_bytes(&mut label, 0..end);
            Label::set_outline(
                &mut label,
                Some(TextOutline {
                    width: 2.,
                    color: crate::peniko::Color::BLACK,
                }),
            );
        });
        root.redraw();
        let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
        assert_eq!(label.active_layout, fitted_layout);
        assert_eq!(
            label.layouts.len(),
            cache_len,
            "outline/reveal reuse shaped layouts"
        );
    }
    root.edit_widget(id, |mut widget| {
        Label::set_text_fit(
            &mut widget.downcast::<Label>(),
            TextFit::Shrink { min_font_size: 24. },
        );
    });
    root.redraw();
    let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
    let layout = &label.layouts[label.active_layout];
    assert_eq!(
        layout.scale, 1.,
        "minimum size cannot enlarge the author's font"
    );
    assert!(
        layout.layout.width() > 210.,
        "minimum-size overflow is explicit"
    );
}

#[test]
fn visible_ranges_keep_utf8_boundaries_bounds_and_empty_range_contract() {
    let text = "aلاe\u{301}";
    for range in [0..0, 1..1, 1..5, 3..5, text.len()..text.len()] {
        let label = Label::new(text).with_visible_bytes(range);
        assert_eq!(label.text().as_ref(), text);
    }
    for range in [
        std::ops::Range { start: 5, end: 1 },
        0..text.len() + 1,
        2..5,
        1..4,
    ] {
        assert!(
            std::panic::catch_unwind(|| Label::new(text).with_visible_bytes(range.clone()))
                .is_err(),
            "invalid visible range must be rejected: {range:?}"
        );
    }
}

#[test]
#[ignore = "requires REEL_SESSION_FONT for a real cached Parley layout"]
fn reveal_changes_paint_without_rebuilding_full_text_layout() {
    let font = Blob::from(
        std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap(),
    );
    let text = "office ffi لا ل\u{64e}ا e\u{301} stable line breaks for full text";
    let label = Label::new(text).with_visible_bytes(0..0).prepare();
    let id = label.id();
    let mut root = RenderRoot::new(
        label,
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(200, 150),
            scale_factor: 1.,
            default_font: Some(font),
        },
    );
    root.redraw();
    let snapshot = |root: &RenderRoot| {
        let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
        let active = label.active_layout;
        let layout = &label.layouts[active].layout;
        (
            label.layouts.len(),
            active,
            label.cache_time,
            layout.width(),
            layout.height(),
            layout
                .lines()
                .map(|line| line.text_range())
                .collect::<Vec<_>>(),
        )
    };
    let before = snapshot(&root);
    let rtl_start = text.find("لا").unwrap();
    for range in [
        0..text.len(),
        rtl_start + 2..rtl_start + 4,
        rtl_start..rtl_start + 4,
        2..3,
        text.len()..text.len(),
    ] {
        root.edit_widget(id, |mut widget| {
            Label::set_visible_bytes(&mut widget.downcast::<Label>(), range)
        });
        root.redraw();
        assert_eq!(snapshot(&root), before);
    }
    assert_eq!(
        root.get_widget(id)
            .unwrap()
            .downcast::<Label>()
            .unwrap()
            .text()
            .as_ref(),
        text
    );
}
