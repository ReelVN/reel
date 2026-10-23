//! Logical inline and block axes for a shaped paragraph.

/// Text flow in physical layout coordinates. Shaping and line breaking retain
/// logical inline/block coordinates; UI consumers map the complete paragraph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WritingMode {
    /// Inline left-to-right or bidi; lines progress downward.
    #[default]
    HorizontalTb,
    /// Inline top-to-bottom; columns progress from right to left.
    VerticalRl,
    /// Inline top-to-bottom; columns progress from left to right.
    VerticalLr,
}

impl WritingMode {
    /// Whether the physical inline axis is vertical.
    pub const fn is_vertical(self) -> bool {
        !matches!(self, Self::HorizontalTb)
    }
    /// Physical sign of increasing logical block coordinates.
    pub(crate) const fn block_sign(self) -> f32 {
        if matches!(self, Self::VerticalRl) {
            -1.
        } else {
            1.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FontContext, FontFamily, Layout, LayoutContext, StyleProperty};
    use alloc::{sync::Arc, vec::Vec};

    fn fonts(bytes: Vec<u8>) -> (FontContext, alloc::string::String) {
        let mut fonts = FontContext {
            collection: crate::fontique::Collection::new(crate::fontique::CollectionOptions {
                system_fonts: false,
                ..Default::default()
            }),
            source_cache: Default::default(),
        };
        let registered = fonts
            .collection
            .register_fonts(linebender_resource_handle::Blob::new(Arc::new(bytes)), None);
        let name = fonts
            .collection
            .family_name(registered[0].0)
            .unwrap()
            .into();
        (fonts, name)
    }

    fn layout(
        fonts: &mut FontContext,
        family: &str,
        source: &str,
        mode: WritingMode,
    ) -> Layout<()> {
        let mut context = LayoutContext::new();
        let mut builder = context.ranged_builder(fonts, source, 1., false);
        builder.push_default(StyleProperty::FontSize(26.));
        builder.push_default(StyleProperty::FontFamily(FontFamily::from(family)));
        builder.set_writing_mode(mode);
        let mut layout = builder.build(source);
        layout.break_all_lines(None);
        layout
    }

    #[test]
    fn missing_vertical_alternate_uses_sideways_grapheme_and_preserves_byte_ranges() {
        let (mut fonts, family) =
            fonts(include_bytes!("../../reel-player/tests/fixtures/font-weight/400.ttf").to_vec());
        let source = "Aー\u{301}B";
        let horizontal = layout(&mut fonts, &family, source, WritingMode::HorizontalTb);
        let vertical = layout(&mut fonts, &family, source, WritingMode::VerticalRl);
        let runs: Vec<_> = vertical.get(0).unwrap().runs().collect();
        assert!(
            runs.iter().all(|run| !run.is_upright()),
            "Tr without alternate rotates with its combining mark"
        );
        let mut ranges: Vec<_> = runs
            .iter()
            .flat_map(|run| run.clusters())
            .map(|cluster| cluster.text_range())
            .collect();
        ranges.sort_by_key(|range| range.start);
        assert_eq!(ranges, [0..1, 1..4, 4..6, 6..7]);
        assert_eq!(vertical.width(), horizontal.width());
    }

    #[test]
    #[ignore = "requires REEL_SESSION_FONT with CJK vertical alternates and Latin; shaping only"]
    fn vertical_cjk_uses_font_alternates_and_bidi_sideways_runs() {
        let (mut fonts, family) = fonts(
            std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real CJK font")).unwrap(),
        );
        let source = "星「ー」ABאב";
        let horizontal = layout(&mut fonts, &family, source, WritingMode::HorizontalTb);
        let vertical = layout(&mut fonts, &family, source, WritingMode::VerticalRl);
        let runs: Vec<_> = vertical.get(0).unwrap().runs().collect();
        assert!(runs.iter().any(|run| run.is_upright()));
        assert!(runs.iter().any(|run| !run.is_upright() && !run.is_rtl()));
        assert!(runs.iter().any(|run| !run.is_upright() && run.is_rtl()));
        let glyph = |layout: &Layout<()>, byte| {
            crate::Cluster::from_byte_index(layout, byte)
                .unwrap()
                .glyphs()
                .next()
                .unwrap()
                .id
        };
        for byte in [0, 3, 6, 9] {
            assert_ne!(glyph(&vertical, byte), 0, "CJK test font must cover source");
        }
        assert_ne!(
            glyph(&horizontal, 6),
            glyph(&vertical, 6),
            "prolonged sound mark uses font's vertical alternate"
        );
        let star = crate::Cluster::from_byte_index(&vertical, 0).unwrap();
        assert!(
            (star.advance() - 26.).abs() < 0.01,
            "upright glyph advances by vertical em"
        );
        let mut ranges: Vec<_> = runs
            .iter()
            .flat_map(|run| run.clusters())
            .map(|cluster| cluster.text_range())
            .collect();
        ranges.sort_by_key(|range| range.start);
        assert_eq!(ranges.first().unwrap().start, 0);
        assert_eq!(ranges.last().unwrap().end, source.len());
        assert!(ranges.windows(2).all(|pair| pair[0].end == pair[1].start));
    }
}
