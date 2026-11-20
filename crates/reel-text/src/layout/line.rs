// Copyright 2021 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::InlineBox;
use crate::layout::Style;
use crate::layout::data::BreakReason;
use crate::layout::data::{LayoutItemKind, LineData};
use crate::layout::glyph::Glyph;
use crate::layout::layout::Layout;
use crate::layout::run::{Run, VisualGlyphs};
use crate::style::Brush;
use core::ops::Range;

/// Line in a text layout.
#[derive(Copy, Clone)]
pub struct Line<'a, B: Brush> {
    pub(crate) layout: &'a Layout<B>,
    pub(crate) index: u32,
    pub(crate) data: &'a LineData,
}

impl<'a, B: Brush> Line<'a, B> {
    /// Returns the metrics for the line.
    pub fn metrics(&self) -> &LineMetrics {
        &self.data.metrics
    }

    pub fn break_reason(&self) -> BreakReason {
        self.data.break_reason
    }

    /// Returns the range of text for the line.
    pub fn text_range(&self) -> Range<usize> {
        self.data.text_range.clone()
    }

    /// Returns the number of items in the line.
    pub fn len(&self) -> usize {
        self.data.item_range.len()
    }

    /// Returns `true` if the line is empty.
    pub fn is_empty(&self) -> bool {
        self.data.item_range.is_empty()
    }

    /// Returns the line item at the specified index.
    pub(crate) fn item(&self, index: usize) -> Option<LineItem<'a, B>> {
        let original_index = index;
        let index = self.data.item_range.start + index;
        if index >= self.data.item_range.end {
            return None;
        }
        let item = self.layout.data.line_items.get(index)?;

        Some(match item.kind {
            LayoutItemKind::TextRun => LineItem::Run(Run {
                layout: self.layout,
                line_index: self.index,
                index: original_index as u32,
                data: self.layout.data.runs.get(item.index)?,
                line_data: Some(item),
            }),
            LayoutItemKind::InlineBox => {
                LineItem::InlineBox(self.layout.data.inline_boxes.get(item.index)?)
            }
        })
    }

    /// Returns an iterator over the runs for the line.
    pub fn runs(&self) -> impl Iterator<Item = Run<'a, B>> + 'a + Clone + use<'a, B> {
        self.items_nonpositioned().filter_map(|item| item.run())
    }

    /// Returns an iterator over the non-glyph runs and inline boxes for the line.
    pub(crate) fn items_nonpositioned(
        &self,
    ) -> impl Iterator<Item = LineItem<'a, B>> + Clone + use<'a, B> {
        let copy = self.clone();
        let line_items = &copy.layout.data.line_items[self.data.item_range.clone()];
        line_items
            .iter()
            .enumerate()
            .map(move |(item_index, line_data)| match line_data.kind {
                LayoutItemKind::TextRun => LineItem::Run(Run {
                    layout: copy.layout,
                    line_index: copy.index,
                    index: item_index as u32,
                    data: &copy.layout.data.runs[line_data.index],
                    line_data: Some(line_data),
                }),
                LayoutItemKind::InlineBox => {
                    LineItem::InlineBox(&copy.layout.data.inline_boxes[line_data.index])
                }
            })
    }

    /// Returns an iterator over the glyph runs for the line.
    pub fn items(
        &self,
    ) -> impl Iterator<Item = PositionedLayoutItem<'a, B>> + 'a + Clone + use<'a, B> {
        GlyphRunIter {
            line: self.clone(),
            item_index: 0,
            glyphs: None,
            next_glyph: None,
            offset: 0.,
        }
    }
}

/// Metrics information for a line.
#[derive(Copy, Clone, Default, Debug, PartialEq)]
pub struct LineMetrics {
    /// Typographic ascent.
    pub ascent: f32,
    /// Typographic descent.
    pub descent: f32,
    /// Typographic leading.
    pub leading: f32,
    /// The absolute line height (in layout units).
    pub line_height: f32,
    /// Offset to the baseline.
    pub baseline: f32,
    /// Offset for alignment.
    pub offset: f32,
    /// Full advance of the line, including trailing whitespace.
    pub advance: f32,
    /// Advance of trailing whitespace.
    pub trailing_whitespace: f32,
    /// Minimum coordinate in the direction orthogonal to line
    /// direction.
    ///
    /// For horizontal text, this would be the top of the line.
    pub min_coord: f32,
    /// Maximum coordinate in the direction orthogonal to line
    /// direction.
    ///
    /// For horizontal text, this would be the bottom of the line.
    pub max_coord: f32,
}

impl LineMetrics {
    /// Returns the size of the line
    pub fn size(&self) -> f32 {
        self.line_height
    }
}

/// A line item and its corresponding data (a run or inline box). Unlike a
/// [`PositionedLayoutItem`], runs are not guaranteed to be split by style.
pub(crate) enum LineItem<'a, B: Brush> {
    Run(Run<'a, B>),
    InlineBox(&'a InlineBox),
}

impl<'a, B: Brush> LineItem<'a, B> {
    pub(crate) fn run(self) -> Option<Run<'a, B>> {
        match self {
            LineItem::Run(run) => Some(run),
            _ => None,
        }
    }
}

/// The computed result of an item (glyph run or inline box) within a layout
#[derive(Clone)]
pub enum PositionedLayoutItem<'a, B: Brush> {
    GlyphRun(GlyphRun<'a, B>),
    InlineBox(PositionedInlineBox),
}

/// The computed position of an inline box within a layout
#[derive(Debug, Clone)]
pub struct PositionedInlineBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub id: u64,
}

/// Sequence of fully positioned glyphs with the same style.
#[derive(Clone)]
pub struct GlyphRun<'a, B: Brush> {
    run: Run<'a, B>,
    style: &'a Style<B>,
    first: Glyph,
    glyphs: VisualGlyphs<'a>,
    glyph_count: usize,
    offset: f32,
    baseline: f32,
    advance: f32,
}

impl<'a, B: Brush> GlyphRun<'a, B> {
    /// Returns the underlying run.
    pub fn run(&self) -> &Run<'a, B> {
        &self.run
    }

    /// Returns the associated style.
    pub fn style(&self) -> &Style<B> {
        self.style
    }

    /// Returns the offset to the baseline.
    pub fn baseline(&self) -> f32 {
        self.baseline
    }

    /// Returns the offset to the first glyph along the baseline.
    pub fn offset(&self) -> f32 {
        self.offset
    }

    /// Returns the total advance of the run.
    pub fn advance(&self) -> f32 {
        self.advance
    }

    /// Returns an iterator over the glyphs in the run.
    pub fn glyphs(&self) -> impl Iterator<Item = Glyph> + 'a + Clone + use<'a, B> {
        core::iter::once(self.first).chain(self.glyphs.clone().take(self.glyph_count - 1))
    }

    /// Returns an iterator over the fully positioned glyphs in the run.
    pub fn positioned_glyphs(&self) -> impl Iterator<Item = Glyph> + 'a + Clone + use<'a, B> {
        let mut offset = self.offset;
        let baseline = self.baseline;
        self.glyphs().map(move |mut g| {
            g.x += offset;
            g.y += baseline;
            offset += g.advance;
            g
        })
    }
}

#[derive(Clone)]
struct GlyphRunIter<'a, B: Brush> {
    line: Line<'a, B>,
    item_index: usize,
    glyphs: Option<VisualGlyphs<'a>>,
    next_glyph: Option<Glyph>,
    offset: f32,
}

impl<'a, B: Brush> Iterator for GlyphRunIter<'a, B> {
    type Item = PositionedLayoutItem<'a, B>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let item = self.line.item(self.item_index)?;
            match item {
                LineItem::InlineBox(inline_box) => {
                    let x = self.offset + self.line.data.metrics.offset;

                    self.item_index += 1;
                    self.offset += inline_box.width;
                    return Some(PositionedLayoutItem::InlineBox(PositionedInlineBox {
                        x,
                        y: self.line.data.metrics.baseline
                            - inline_box.baseline.unwrap_or(inline_box.height),
                        width: inline_box.width,
                        height: inline_box.height,
                        id: inline_box.id,
                    }));
                }
                LineItem::Run(run) => {
                    let iter = self.glyphs.get_or_insert_with(|| run.visual_glyphs());
                    if let Some(first) = self.next_glyph.take().or_else(|| iter.next()) {
                        let glyphs = iter.clone();
                        let style_index = first.style_index();
                        let (glyph_count, advance) = if run.layout.data.styles.len() == 1 {
                            self.item_index += 1;
                            self.glyphs = None;
                            glyphs
                                .clone()
                                .fold((1, first.advance), |(count, advance), glyph| {
                                    (count + 1, advance + glyph.advance)
                                })
                        } else {
                            let mut advance = first.advance;
                            let mut glyph_count = 1;
                            for glyph in iter.by_ref() {
                                if glyph.style_index() != style_index {
                                    self.next_glyph = Some(glyph);
                                    break;
                                }
                                glyph_count += 1;
                                advance += glyph.advance;
                            }
                            (glyph_count, advance)
                        };
                        let style = run.layout.data.styles.get(style_index)?;
                        let offset = self.offset;
                        self.offset += advance;
                        return Some(PositionedLayoutItem::GlyphRun(GlyphRun {
                            run,
                            style,
                            first,
                            glyphs,
                            glyph_count,
                            offset: offset + self.line.data.metrics.offset,
                            baseline: self.line.data.metrics.baseline,
                            advance,
                        }));
                    }
                    self.item_index += 1;
                    self.glyphs = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FontContext, LayoutContext, StyleProperty};

    #[test]
    fn styled_glyph_runs_read_only_their_visual_span() {
        let mut fonts = FontContext::new();
        fonts.collection.register_fonts(
            linebender_resource_handle::Blob::new(alloc::sync::Arc::new(
                include_bytes!("../../../reel-player/tests/fixtures/font-weight/400.ttf").to_vec(),
            )),
            None,
        );
        let source = "AAAAAAA";
        let mut context = LayoutContext::new();
        let mut builder = context.ranged_builder(&mut fonts, source, 1., true);
        builder.push(StyleProperty::Brush(1u32), 1..3);
        builder.push(StyleProperty::Brush(2), 4..6);
        let mut layout = builder.build(source);
        layout.break_all_lines(None);
        let line = layout.get(0).unwrap();
        let mut expected = alloc::vec::Vec::new();
        for run in line.runs() {
            expected.extend(run.visual_clusters().flat_map(|cluster| cluster.glyphs()));
        }
        assert_eq!(expected.len(), 7);
        let mut offset = 0.;
        let mut glyph_index = 0;
        let mut brushes = alloc::vec::Vec::new();
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(run) = item else {
                panic!("plain line contains an inline box");
            };
            assert_eq!(run.offset(), offset);
            brushes.push(run.style().brush);
            for glyph in run.glyphs() {
                assert_eq!(glyph, expected[glyph_index]);
                glyph_index += 1;
            }
            offset += run.advance();
        }
        assert_eq!(glyph_index, expected.len());
        assert_eq!(brushes, [0, 1, 0, 2, 0]);
    }
}
