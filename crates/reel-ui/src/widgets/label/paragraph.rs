//! One shaped paragraph with ranged styles and baseline-aligned ruby boxes.
use crate::{
    core::{BrushIndex, StyleProperty, StyleSet, TextOutline, render_text, render_text_revealed},
    imaging::Painter,
    kurbo::Affine,
    peniko::{Brush, Color},
    reel_text::{self, FontContext, Layout, LayoutContext, PositionedLayoutItem},
};
use reel_core::text::{Text, TextStyle};
use std::ops::Range;

#[derive(Default)]
pub(super) struct Paragraph {
    layout: Layout<BrushIndex>,
    boxes: Vec<Annotation>,
    mapping: Vec<Mapping>,
    brushes: Vec<Brush>,
    plain: Range<usize>,
    text: String,
    source: Text,
    emphasis: bool,
    default_color: Option<Brush>,
}
struct Mapping {
    plain: Range<usize>,
    start: usize,
}
struct Annotation {
    range: Range<usize>,
    index: usize,
    base: Paragraph,
    note: Paragraph,
    width: f32,
    baseline: f32,
    height: f32,
}
impl std::ops::Deref for Paragraph {
    type Target = Layout<BrushIndex>;
    fn deref(&self) -> &Self::Target {
        &self.layout
    }
}
impl std::ops::DerefMut for Paragraph {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.layout
    }
}
impl Paragraph {
    pub fn build(
        fonts: &mut FontContext,
        context: &mut LayoutContext<BrushIndex>,
        text: &Text,
        styles: &StyleSet,
        width: Option<f32>,
        scale: f32,
        quantize: bool,
    ) -> Self {
        Self::fragment(
            fonts,
            context,
            text,
            styles,
            0..text.len(),
            true,
            true,
            width,
            scale,
            quantize,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn fragment(
        fonts: &mut FontContext,
        context: &mut LayoutContext<BrushIndex>,
        text: &Text,
        styles: &StyleSet,
        range: Range<usize>,
        ruby: bool,
        emphasis: bool,
        width: Option<f32>,
        scale: f32,
        quantize: bool,
    ) -> Self {
        let mut out = Self {
            plain: range.clone(),
            source: text.clone(),
            emphasis: emphasis
                && text.spans().iter().any(|s| {
                    s.style.emphasis && s.range.start < range.end && range.start < s.range.end
                }),
            brushes: vec![Color::WHITE.into()],
            ..Default::default()
        };
        let mut cursor = range.start;
        for annotation in text
            .ruby()
            .iter()
            .filter(|r| ruby && range.start <= r.range.start && r.range.end <= range.end)
        {
            let unit = annotation.range.clone();
            let note = annotation.annotation.as_ref();
            out.append(text, cursor..unit.start);
            let base = Self::fragment(
                fonts,
                context,
                text,
                styles,
                unit.clone(),
                false,
                true,
                None,
                scale,
                quantize,
            );
            let mut note_styles = styles.clone();
            let base_size = text
                .style_at(unit.start)
                .and_then(|s| s.size)
                .unwrap_or_else(|| {
                    styles
                        .inner()
                        .values()
                        .find_map(|p| {
                            if let StyleProperty::FontSize(n) = p {
                                Some(*n)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(24.)
                });
            note_styles.insert(StyleProperty::FontSize(base_size * 0.5));
            if text.style_at(unit.start).is_some_and(|s| s.bold) {
                note_styles.insert(StyleProperty::FontWeight(reel_text::FontWeight::BOLD));
            }
            let mut note = Self::fragment(
                fonts,
                context,
                &Text::from(note),
                &note_styles,
                0..note.len(),
                false,
                false,
                None,
                scale,
                quantize,
            );
            if let Some(color) = text.style_at(unit.start).and_then(|s| s.color) {
                note.default_color = Some(brush(color));
            }
            let baseline = note.height()
                + base
                    .get(0)
                    .map_or(base.height(), |line| line.metrics().baseline);
            let height = note.height() + base.height();
            let width = base.width().max(note.width());
            out.boxes.push(Annotation {
                range: unit.clone(),
                index: out.text.len(),
                base,
                note,
                width,
                height,
                baseline,
            });
            cursor = unit.end;
        }
        out.append(text, cursor..range.end);
        let mut builder = context.ranged_builder(fonts, &out.text, scale, quantize);
        for property in styles.inner().values() {
            builder.push_default(property.clone());
        }
        for piece in &out.mapping {
            let first = text
                .spans()
                .partition_point(|s| s.range.end <= piece.plain.start);
            for span in &text.spans()[first..] {
                if span.range.start >= piece.plain.end {
                    break;
                }
                let start =
                    span.range.start.max(piece.plain.start) - piece.plain.start + piece.start;
                let end = span.range.end.min(piece.plain.end) - piece.plain.start + piece.start;
                apply(
                    &mut builder,
                    &span.style,
                    styles,
                    start..end,
                    &mut out.brushes,
                );
            }
        }
        for (id, item) in out.boxes.iter().enumerate() {
            builder.push_inline_box(reel_text::InlineBox {
                id: id as u64,
                index: item.index,
                width: item.width,
                height: item.height,
                baseline: Some(item.baseline),
            });
        }
        builder.build_into(&mut out.layout, &out.text);
        out.layout.break_all_lines(width);
        out
    }
    fn append(&mut self, source: &str, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        self.mapping.push(Mapping {
            plain: range.clone(),
            start: self.text.len(),
        });
        self.text.push_str(&source[range]);
    }
    fn position(&self, plain: usize) -> usize {
        if plain <= self.plain.start {
            return 0;
        }
        if plain >= self.plain.end {
            return self.text.len();
        }
        let i = self.mapping.partition_point(|m| m.plain.start <= plain);
        if let Some(piece) = i.checked_sub(1).map(|i| &self.mapping[i]) {
            piece.start + plain.min(piece.plain.end) - piece.plain.start
        } else {
            0
        }
    }
    pub fn paint(
        &mut self,
        painter: &mut Painter<'_>,
        transform: Affine,
        color: Brush,
        hint: bool,
        visible: Range<usize>,
        outline: Option<TextOutline>,
    ) {
        self.brushes[0] = self.default_color.clone().unwrap_or_else(|| color.clone());
        let range = self.position(visible.start)..self.position(visible.end);
        if range.start == 0 && range.end == self.text.len() {
            render_text(
                painter,
                transform,
                &self.layout,
                &self.brushes,
                hint,
                outline,
            );
        } else {
            render_text_revealed(
                painter,
                transform,
                &self.layout,
                &self.brushes,
                hint,
                range,
                outline,
            );
        }
        if self.emphasis {
            let shown = self.position(visible.start)..self.position(visible.end);
            for line in self.layout.lines() {
                let mut previous = None;
                for item in line.items() {
                    let PositionedLayoutItem::GlyphRun(glyphs) = item else {
                        continue;
                    };
                    let run = glyphs.run();
                    if previous == Some(run.index()) {
                        continue;
                    }
                    previous = Some(run.index());
                    let masks = crate::core::revealed_clusters(run, &shown);
                    let mut advance = glyphs.offset();
                    for (index, cluster) in run.visual_clusters().enumerate() {
                        let x = advance + cluster.advance() / 2.;
                        advance += cluster.advance();
                        let logical = run.visual_to_logical(index).unwrap();
                        if !masks[logical] {
                            continue;
                        }
                        let range = cluster.text_range();
                        let Some(offset) = self.plain_position(range.start) else {
                            continue;
                        };
                        let end = offset + range.len();
                        let Some(style) = self.source.style_at(offset).filter(|s| s.emphasis)
                        else {
                            continue;
                        };
                        if offset < visible.start
                            || end > visible.end
                            || self.source[offset..end].chars().all(char::is_whitespace)
                        {
                            continue;
                        }
                        let y =
                            line.metrics().baseline - run.metrics().ascent - run.font_size() * 0.14;
                        let color = style.color.map(brush).unwrap_or_else(|| color.clone());
                        painter
                            .fill(
                                crate::kurbo::Circle::new(
                                    (x as f64, y as f64),
                                    run.font_size() as f64 * 0.045,
                                ),
                                &color,
                            )
                            .transform(transform)
                            .draw();
                    }
                }
            }
        }
        for line in self.layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::InlineBox(position) = item else {
                    continue;
                };
                let item = &mut self.boxes[position.id as usize];
                let base_x = position.x + (item.width - item.base.width()) / 2.;
                let base_y = position.y + item.note.height();
                item.base.paint(
                    painter,
                    transform * Affine::translate((base_x as f64, base_y as f64)),
                    color.clone(),
                    hint,
                    visible.clone(),
                    outline,
                );
                if visible.start <= item.range.start && item.range.end <= visible.end {
                    let note_x = position.x + (item.width - item.note.width()) / 2.;
                    item.note.paint(
                        painter,
                        transform * Affine::translate((note_x as f64, position.y as f64)),
                        color.clone(),
                        hint,
                        0..usize::MAX,
                        outline,
                    );
                }
            }
        }
    }
    pub fn paint_focus(
        &self,
        painter: &mut Painter<'_>,
        transform: Affine,
        range: Range<usize>,
        color: Brush,
    ) {
        use reel_text::{Affinity, Cursor, Selection};
        let start = self.position(range.start);
        let end = self.position(range.end);
        if start < end {
            let selection = Selection::new(
                Cursor::from_byte_index(&self.layout, start, Affinity::Downstream),
                Cursor::from_byte_index(&self.layout, end, Affinity::Upstream),
            );
            selection.geometry_with(&self.layout, |rect, _| {
                painter
                    .fill(
                        crate::kurbo::Rect::new(rect.x0, rect.y1 - 2., rect.x1, rect.y1),
                        &color,
                    )
                    .transform(transform)
                    .draw();
            });
        }
        for line in self.layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::InlineBox(p) = item else {
                    continue;
                };
                let b = &self.boxes[p.id as usize];
                if b.range.start < range.end && range.start < b.range.end {
                    b.base.paint_focus(
                        painter,
                        transform
                            * Affine::translate((
                                (p.x + (b.width - b.base.width()) / 2.) as f64,
                                (p.y + b.note.height()) as f64,
                            )),
                        range.clone(),
                        color.clone(),
                    );
                }
            }
        }
    }
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        for line in self.layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::InlineBox(p) = item else {
                    continue;
                };
                if x < p.x || x >= p.x + p.width || y < p.y || y >= p.y + p.height {
                    continue;
                }
                let b = &self.boxes[p.id as usize];
                if y < p.y + b.note.height() {
                    return Some(b.range.start);
                }
                return b.base.hit(
                    x - p.x - (b.width - b.base.width()) / 2.,
                    y - p.y - b.note.height(),
                );
            }
        }
        let (cluster, _) = reel_text::Cluster::from_point_exact(&self.layout, x, y)?;
        self.plain_position(cluster.text_range().start)
    }
    fn plain_position(&self, offset: usize) -> Option<usize> {
        let i = self
            .mapping
            .partition_point(|m| m.start <= offset)
            .checked_sub(1)?;
        Some(self.mapping[i].plain.start + offset - self.mapping[i].start)
    }
}
fn brush([r, g, b, a]: [u8; 4]) -> Brush {
    Color::from_rgba8(r, g, b, a).into()
}
fn apply(
    builder: &mut reel_text::RangedBuilder<'_, BrushIndex>,
    style: &TextStyle,
    defaults: &StyleSet,
    range: Range<usize>,
    brushes: &mut Vec<Brush>,
) {
    if style.bold {
        builder.push(
            StyleProperty::FontWeight(reel_text::FontWeight::BOLD),
            range.clone(),
        );
    }
    if style.italic {
        builder.push(
            StyleProperty::FontStyle(reel_text::FontStyle::Italic),
            range.clone(),
        );
    }
    if style.underline {
        builder.push(StyleProperty::Underline(true), range.clone());
    }
    if style.strike {
        builder.push(StyleProperty::Strikethrough(true), range.clone());
    }
    if style.emphasis {
        let height = defaults
            .inner()
            .values()
            .find_map(|p| match p {
                StyleProperty::LineHeight(h) => Some(*h),
                _ => None,
            })
            .unwrap_or_default();
        let height = match height {
            reel_text::LineHeight::FontSizeRelative(h) => {
                reel_text::LineHeight::FontSizeRelative(h.max(1.8))
            }
            reel_text::LineHeight::MetricsRelative(h) => {
                reel_text::LineHeight::MetricsRelative(h.max(1.8))
            }
            reel_text::LineHeight::Absolute(h) => {
                let size = style.size.unwrap_or_else(|| {
                    defaults
                        .inner()
                        .values()
                        .find_map(|p| match p {
                            StyleProperty::FontSize(s) => Some(*s),
                            _ => None,
                        })
                        .unwrap_or(24.)
                });
                reel_text::LineHeight::Absolute(h.max(size * 1.8))
            }
        };
        builder.push(StyleProperty::LineHeight(height), range.clone());
    }
    if let Some(size) = style.size {
        builder.push(StyleProperty::FontSize(size), range.clone());
    }
    if let Some(color) = style.color {
        let index = brushes.len();
        brushes.push(brush(color));
        builder.push(StyleProperty::Brush(BrushIndex(index)), range);
    }
}
