//! CPU paragraph, glyph-record and hit-geometry checks; no window is opened.
use super::*;
use crate::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    core::{DefaultProperties, NewWidget},
    dpi::PhysicalSize,
    imaging::record::{Command, Draw},
    peniko::Blob,
};
use std::sync::Arc;

fn root(label: NewWidget<Label>, width: u32, height: u32) -> RenderRoot {
    let font = Blob::from(
        std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real CJK font")).unwrap(),
    );
    RenderRoot::new(label, |_| {}, RenderRootOptions {
        default_properties: Arc::new(DefaultProperties::new()),
        use_system_fonts: false,
        size_policy: WindowSizePolicy::User,
        size: PhysicalSize::new(width, height),
        scale_factor: 1.,
        default_font: Some(font),
    })
}

#[test]
#[ignore = "requires REEL_SESSION_FONT with CJK and Latin; CPU glyph records only"]
fn vertical_columns_wrap_at_height_and_keep_upright_and_sideways_outlines() {
    for mode in [crate::WritingMode::VerticalRl, crate::WritingMode::VerticalLr] {
        let label = NewWidget::new(Label::new("星空AB\n星空AB")
            .with_writing_mode(mode)
            .with_style(StyleProperty::FontSize(26.))
            .with_style(StyleProperty::LineHeight(reel_text::LineHeight::Absolute(40.))))
            .with_props(LineBreaking::WordWrap);
        let id = label.id();
        let mut root = root(label, 180, 120);
        let (plan, _) = root.redraw();
        let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
        let layout = &label.layouts[label.active_layout].layout;
        assert_eq!(layout.writing_mode(), mode);
        assert_eq!(layout.len(), 2);
        assert!(layout.width() <= 120. && layout.width() > 52.);
        let first = layout.get(0).unwrap().metrics().baseline;
        let second = layout.get(1).unwrap().metrics().baseline;
        assert_eq!(second - first, 40.);
        let p0 = label.axes() * Point::new(0., first as f64);
        let p1 = label.axes() * Point::new(0., second as f64);
        assert_eq!(p1.x - p0.x, if mode == crate::WritingMode::VerticalRl { -40. } else { 40. });
        assert_eq!(p0.y, 0.);

        let orientations: Vec<_> = layout.lines().flat_map(|line| line.runs())
            .map(|run| (run.is_upright(), run.text_range())).collect();
        assert!(orientations.iter().any(|(upright, range)| *upright && range.start == 0));
        assert!(orientations.iter().any(|(upright, range)| !*upright && range.start == 6));
        let flip = Affine::scale_non_uniform(1., -1.);
        let rotation = Affine::new([0., 1., -1., 0., 0., 0.]);
        let mut upright = false;
        let mut sideways = false;
        for layer in &plan.layers {
            let crate::app::VisualLayerKind::Scene(scene) = &layer.kind else { continue };
            for command in scene.commands() {
                let Command::Draw(id) = command else { continue };
                let Draw::GlyphRun(run) = scene.draw_op(*id) else { continue };
                let glyph_axes = run.glyph_transform.expect("vertical glyph compensation");
                let actual = run.transform * flip * glyph_axes;
                // Remove paragraph translation and compare physical basis vectors.
                for (expected, found) in [(flip, &mut upright), (rotation * flip, &mut sideways)] {
                    if actual.as_coeffs()[..4] == expected.as_coeffs()[..4] {
                        *found = true;
                    }
                }
            }
        }
        assert!(upright && sideways, "physical outlines must be upright or clockwise, never inverted");
        let first_ink = label.axes() * Point::new(13., first as f64);
        assert!(first_ink.x >= 0. && first_ink.x <= 180. && first_ink.y == 13.);
        let endpoint = label.text_end().unwrap();
        assert!(endpoint.is_finite() && endpoint.y > 0. && endpoint.y <= 120.);
    }
}

#[test]
#[ignore = "requires REEL_SESSION_FONT with CJK and Latin; CPU ruby/link/fade geometry only"]
fn vertical_ruby_reveal_links_outline_shadow_and_viewport_share_cached_geometry() {
    for mode in [crate::WritingMode::VerticalRl, crate::WritingMode::VerticalLr] {
        let source = Text::parse("[link=entry][ruby=ほし]星[/ruby]空[/link]\n次の列").unwrap();
        let label = NewWidget::new(Label::new("")
            .with_rich_text(source.clone())
            .with_writing_mode(mode)
            .with_style(StyleProperty::FontSize(26.))
            .with_outline(Some(TextOutline { width: 1., color: crate::peniko::Color::BLACK }))
            .with_shadow(Some(TextShadow { offset: crate::kurbo::Vec2::new(2., 3.), color: crate::peniko::Color::BLACK }))
            .with_visible_bytes(0..0)).with_props(LineBreaking::WordWrap);
        let id = label.id();
        let mut root = root(label, 180, 160);
        root.redraw();
        let (base_point, note_point, cache) = {
            let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
            let paragraph = &label.layouts[label.active_layout].layout;
            let position = paragraph.get(0).unwrap().items().find_map(|item| match item {
                reel_text::PositionedLayoutItem::InlineBox(p) => Some(p), _ => None,
            }).expect("ruby is one inline box");
            let note_height = position.height / 3.;
            let (base_y, note_y) = if mode == crate::WritingMode::VerticalLr {
                (position.y + note_height, position.y + position.height - note_height / 2.)
            } else {
                (position.y + position.height - note_height, position.y + note_height / 2.)
            };
            let base = label.axes() * Point::new((position.x + position.width / 2.) as f64, base_y as f64);
            let note = label.axes() * Point::new((position.x + position.width / 2.) as f64, note_y as f64);
            assert!(note.x > base.x, "ruby is physically right of its base in both column directions");
            assert_eq!(label.link_at(base), None);
            assert_eq!(label.link_at(note), None);
            (base, note, (label.cache_time, label.active_layout, label.layouts.len()))
        };
        root.edit_widget(id, |mut widget| Label::set_visible_bytes(&mut widget.downcast::<Label>(), 0..source.len()));
        let (plan, _) = root.redraw();
        {
            let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
            assert_eq!(label.link_at(base_point), Some(0));
            assert_eq!(label.link_at(note_point), Some(0));
            assert_eq!(cache, (label.cache_time, label.active_layout, label.layouts.len()));
        }
        let mut records = Vec::new();
        for layer in &plan.layers {
            let crate::app::VisualLayerKind::Scene(scene) = &layer.kind else { continue };
            for command in scene.commands() {
                if let Command::Draw(id) = command && let Draw::GlyphRun(run) = scene.draw_op(*id) {
                    records.push(run);
                }
            }
        }
        assert!(!records.is_empty());
        assert!(records.iter().any(|run| matches!(run.style, crate::peniko::Style::Stroke(_))));
        for foreground in records.iter().filter(|run| run.brush != crate::peniko::Brush::Solid(crate::peniko::Color::BLACK)) {
            assert!(records.iter().any(|shadow| shadow.glyphs == foreground.glyphs
                && shadow.glyph_transform == foreground.glyph_transform
                && shadow.transform == Affine::translate((2., 3.)) * foreground.transform));
        }
        root.edit_widget(id, |mut widget| {
            let mut label = widget.downcast::<Label>();
            Label::set_reveal_fade(&mut label, Some(crate::core::RevealFade {from: 0, progress: 0.25, step: 0.25}));
        });
        root.redraw();
        let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
        assert_eq!(cache, (label.cache_time, label.active_layout, label.layouts.len()));
        assert_eq!(label.link_at(base_point), None, "a zero-opacity link tail keeps the link inactive");
        let paragraph = &label.layouts[label.active_layout].layout;
        let strip = Rect::new(base_point.x - 1., 0., base_point.x + 1., 160.);
        assert_eq!(paragraph.visible_lines(Some(strip), label.axes(), label.outline), 0..1);
    }
}
