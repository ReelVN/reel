//! Full advances size fragments and fit text; alignment retains visible-line semantics.
use super::*;
use crate::peniko::Blob;

fn contexts() -> (FontContext, LayoutContext<BrushIndex>, String) {
    let mut fonts = FontContext {
        collection: reel_text::fontique::Collection::new(reel_text::fontique::CollectionOptions {
            system_fonts: false,
            ..Default::default()
        }),
        source_cache: Default::default(),
    };
    let blob = if let Some(path) = std::env::var_os("REEL_SESSION_FONT") {
        std::fs::read(path).unwrap().into()
    } else {
        Blob::from(
            include_bytes!(
                "../../../../../examples/author_arcade/resources/fonts/AtkinsonHyperlegible-Regular.ttf"
            )
            .to_vec(),
        )
    };
    let family = fonts.collection.register_fonts(blob, None)[0].0;
    let name = fonts.collection.family_name(family).unwrap().to_owned();
    (fonts, LayoutContext::new(), name)
}

#[test]
fn trailing_space_caret_survives_visible_alignment_and_wrapping() {
    let (mut fonts, mut context, family) = contexts();
    for source in ["Add ", "  Add   ", "   ", "Add \nA  ", "Add Add Add  "] {
        let mut label = Label::new(source)
            .with_style(StyleProperty::FontSize(24.))
            .with_style(StyleProperty::FontFamily(
                reel_text::FontFamily::from(family.as_str()).into_owned(),
            ));
        for width in [None, Some(50.)] {
            let index = label.build_and_break(&mut fonts, &mut context, width);
            let layout = &mut label.layouts[index];
            assert!(layout.layout.full_width() > layout.layout.width());
            if width.is_some() && source == "Add Add Add  " {
                assert!(layout.layout.len() > 1, "automatic wrapping is exercised");
            }
            for alignment in [TextAlign::Start, TextAlign::Center, TextAlign::End] {
                layout.align(alignment, 180.);
                let last = layout.layout.lines().last().unwrap();
                let line = last.metrics();
                let visible = line.advance - line.trailing_whitespace;
                let expected_offset = match alignment {
                    TextAlign::Center => (180. - visible) / 2.,
                    TextAlign::End => 180. - visible,
                    _ => 0.,
                };
                assert!((line.offset - expected_offset).abs() < 0.001);
                let caret = layout.layout.text_caret().unwrap();
                assert!((caret.p0.x - (line.offset + line.advance) as f64).abs() < 0.001);
                if alignment == TextAlign::End {
                    assert!(
                        caret.p0.x > 180.,
                        "trailing whitespace hangs beyond visible alignment"
                    );
                }
            }
        }
    }
}

#[test]
fn shrink_fits_full_advance_including_space_only_and_explicit_lines() {
    let (mut fonts, mut context, family) = contexts();
    for source in ["Add       ", "         ", "A\nAdd       "] {
        let mut label = Label::new(source)
            .with_style(StyleProperty::FontSize(24.))
            .with_style(StyleProperty::FontFamily(
                reel_text::FontFamily::from(family.as_str()).into_owned(),
            ))
            .with_text_fit(TextFit::Shrink { min_font_size: 0. });
        let natural = label.build_and_break(&mut fonts, &mut context, None);
        let paragraph = &label.layouts[natural].layout;
        let width = (paragraph.width() + paragraph.full_width()) / 2.;
        let lines = paragraph.len();
        let fitted = label.build_and_break(&mut fonts, &mut context, Some(width));
        let layout = &label.layouts[fitted];
        assert!(layout.scale < 1.);
        assert!(layout.layout.full_width() <= width + 0.01);
        assert_eq!(
            layout.layout.len(),
            lines,
            "shrink preserves explicit lines"
        );
        assert_eq!(
            label.build_and_break(&mut fonts, &mut context, Some(width)),
            fitted
        );
    }
}
