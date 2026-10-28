use super::*;

fn fonts(bytes: Vec<u8>) -> (FontContext, StyleSet) {
    let mut fonts = FontContext {
        collection: reel_text::fontique::Collection::new(reel_text::fontique::CollectionOptions {
            system_fonts: false,
            ..Default::default()
        }),
        source_cache: Default::default(),
    };
    let families = fonts
        .collection
        .register_fonts(crate::peniko::Blob::from(bytes), None);
    let family = fonts
        .collection
        .family_name(families[0].0)
        .unwrap()
        .to_owned();
    let mut styles = StyleSet::new(24.);
    default_text_styles(&mut styles);
    styles.insert(StyleProperty::FontFamily(
        reel_text::FontFamily::from(family.as_str()).into_owned(),
    ));
    styles.insert(StyleProperty::FontSize(24.));
    (fonts, styles)
}

#[test]
#[ignore = "CPU-only history reflow cost; requires REEL_SESSION_FONT"]
fn history_width_reflow_cost() {
    let bytes = std::fs::read(std::env::var_os("REEL_SESSION_FONT").unwrap()).unwrap();
    let (mut fonts, styles) = fonts(bytes);
    let mut context = LayoutContext::new();
    let text = "History entry: the train reached the station while the conversation continued. ffi office العربية שלום. ".repeat(32);
    let started = std::time::Instant::now();
    let mut line_count = 0;
    for _ in 0..40 {
        let mut label = Label::new(text.as_str());
        label.styles = styles.clone();
        for width in [
            None,
            Some(680.),
            Some(590.),
            Some(880.),
            Some(710.),
            Some(570.),
            Some(640.),
            Some(780.),
        ] {
            let index = label.build_scaled(&mut fonts, &mut context, width, 1.);
            label.active_layout = index;
            label.layouts[index].align(TextAlign::Justify, width.unwrap_or(1000.));
            line_count += label.layouts[index].layout.len();
        }
    }
    println!(
        "history_reflow paragraphs=40 widths=8 bytes={} lines={line_count} elapsed_us={}",
        text.len(),
        started.elapsed().as_micros()
    );
}

fn assert_paragraph(actual: &Paragraph, expected: &Paragraph) {
    let close = |a: f32, b: f32| assert!((a - b).abs() < 0.002, "{a} != {b}");
    assert_eq!(actual.len(), expected.len());
    close(actual.width(), expected.width());
    close(actual.height(), expected.height());
    for (a, b) in actual.lines().zip(expected.lines()) {
        assert_eq!(a.text_range(), b.text_range());
        close(a.metrics().baseline, b.metrics().baseline);
        close(a.metrics().advance, b.metrics().advance);
        let ag: Vec<_> = a.items().collect();
        let bg: Vec<_> = b.items().collect();
        assert_eq!(ag.len(), bg.len());
        for (a, b) in ag.into_iter().zip(bg) {
            match (a, b) {
                (
                    reel_text::PositionedLayoutItem::GlyphRun(a),
                    reel_text::PositionedLayoutItem::GlyphRun(b),
                ) => {
                    close(a.offset(), b.offset());
                    close(a.baseline(), b.baseline());
                    let ag: Vec<_> = a.glyphs().collect();
                    let bg: Vec<_> = b.glyphs().collect();
                    assert_eq!(ag.len(), bg.len());
                    for (a, b) in ag.into_iter().zip(bg) {
                        assert_eq!((a.id, a.style_index), (b.id, b.style_index));
                        close(a.x, b.x);
                        close(a.y, b.y);
                        close(a.advance, b.advance);
                    }
                }
                (
                    reel_text::PositionedLayoutItem::InlineBox(a),
                    reel_text::PositionedLayoutItem::InlineBox(b),
                ) => {
                    assert_eq!(a.id, b.id);
                    close(a.x, b.x);
                    close(a.y, b.y);
                    close(a.width, b.width);
                    close(a.height, b.height);
                }
                _ => panic!("paragraph item kinds differ"),
            }
        }
    }
    for y in (0..actual.height() as usize).step_by(11) {
        let viewport = Some(Rect::new(0., y as f64, 160., y as f64 + 30.));
        assert_eq!(
            actual.visible_lines(viewport, Affine::IDENTITY, None),
            expected.visible_lines(viewport, Affine::IDENTITY, None)
        );
        assert_eq!(actual.hit(31., y as f32), expected.hit(31., y as f32));
    }
    assert_eq!(actual.text_caret(), expected.text_caret());
}

fn reflow_contract(bytes: Vec<u8>) {
    let (mut fonts, styles) = fonts(bytes);
    let mut context = LayoutContext::new();
    let source = Text::parse("[link=entry][ruby=reading][b]Base[/b][/ruby][/link] [size=36]Large[/size] [em]ffi marked text[/em] العربية שלום. ".repeat(8)).unwrap();
    for mode in [
        crate::WritingMode::HorizontalTb,
        crate::WritingMode::VerticalRl,
        crate::WritingMode::VerticalLr,
    ] {
        let mut label = Label::new("")
            .with_rich_text(source.clone())
            .with_writing_mode(mode);
        label.styles = styles.clone();
        for width in [320., 160., 280., 120., 240., 360., 220.] {
            let index = label.build_scaled(&mut fonts, &mut context, Some(width), 1.);
            label.active_layout = index;
            for alignment in [TextAlign::Justify, TextAlign::Center, TextAlign::Start] {
                label.layouts[index].align(alignment, width);
                let mut expected = Paragraph::build(
                    &mut fonts,
                    &mut context,
                    &source,
                    &styles,
                    Some(width),
                    1.,
                    true,
                    mode,
                );
                expected.align(Some(width), alignment, TextAlignOptions::default());
                assert_paragraph(&label.layouts[index].layout, &expected);
            }
            // Leave an aligned paragraph in the cache for the following reflow.
            label.layouts[index].align(TextAlign::Justify, width);
        }
    }
}

#[test]
fn cached_width_changes_match_fresh_ruby_and_vertical_shaping() {
    reflow_contract(
        include_bytes!("../../../../reel-player/tests/fixtures/font-weight/400.ttf").to_vec(),
    );
}

#[test]
#[ignore = "requires REEL_SESSION_FONT with Latin, Arabic and Hebrew shaping"]
fn real_font_width_changes_match_fresh_shaping() {
    reflow_contract(std::fs::read(std::env::var_os("REEL_SESSION_FONT").unwrap()).unwrap());
}
