//! One shaped paragraph with ranged styles and baseline-aligned ruby boxes.
use crate::{
    core::{
        BrushIndex, RevealFade, StyleProperty, StyleSet, TextOutline, render_text_faded_lines,
        render_text_lines,
    },
    imaging::Painter,
    kurbo::{Affine, Point, Rect},
    peniko::{Brush, Color},
    reel_text::{self, FontContext, Layout, LayoutContext, PositionedLayoutItem},
};
use reel_core::text::{Text, TextStyle};
use skrifa::{
    FontRef, MetadataProvider,
    instance::{LocationRef, Size},
};
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
    // Prefix maximum bottoms and suffix minimum tops give a conservative
    // searchable range even when large glyphs or ruby overlap adjacent lines.
    paint_lines: Vec<(f32, f32)>,
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
    pub fn text_end(&self) -> Option<Point> {
        if self.plain.is_empty() {
            return None;
        }
        if let Some(annotation) = self
            .boxes
            .last()
            .filter(|item| item.range.end == self.plain.end)
        {
            let line = self.layout.get(self.layout.len().checked_sub(1)?)?;
            let id = (self.boxes.len() - 1) as u64;
            for item in line.items() {
                if let PositionedLayoutItem::InlineBox(position) = item
                    && position.id == id
                {
                    let x = if annotation.base.is_rtl() {
                        position.x
                    } else {
                        position.x + position.width
                    };
                    return Some(Point::new(x as f64, line.metrics().max_coord as f64));
                }
            }
        }
        let caret = reel_text::Cursor::from_byte_index(
            &self.layout,
            self.text.len(),
            reel_text::Affinity::Upstream,
        )
        .geometry(&self.layout, 0.);
        Some(Point::new(caret.x0, caret.y1))
    }

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
            if let Some(family) = text.style_at(unit.start).and_then(|s| s.font.as_deref()) {
                note_styles.insert(StyleProperty::FontFamily(
                    reel_text::FontFamily::from(family).into_owned(),
                ));
            }
            if let Some(weight) = text
                .style_at(unit.start)
                .and_then(|s| s.weight.or_else(|| s.bold.then_some(700.)))
            {
                note_styles.insert(StyleProperty::FontWeight(reel_text::FontWeight::new(
                    weight,
                )));
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
        out.index_paint_lines();
        out
    }

    fn index_paint_lines(&mut self) {
        let mut last_font = None;
        let mut font_bounds = None;
        for line in self.layout.lines() {
            let metrics = line.metrics();
            let mut top = metrics.min_coord;
            let mut bottom = metrics.max_coord;
            for item in line.items() {
                match item {
                    PositionedLayoutItem::GlyphRun(glyphs) => {
                        let run = glyphs.run();
                        let font = run.font();
                        let key = (font.data.id(), font.index, run.font_size().to_bits());
                        if last_font != Some(key) {
                            last_font = Some(key);
                            font_bounds = FontRef::from_index(font.data.as_ref(), font.index)
                                .ok()
                                .and_then(|font| {
                                    font.metrics(Size::new(run.font_size()), LocationRef::default())
                                        .bounds
                                });
                        }
                        if let Some(bounds) = font_bounds {
                            for glyph in glyphs.glyphs() {
                                let y = glyphs.baseline() + glyph.y;
                                top = top.min(y - bounds.y_max);
                                bottom = bottom.max(y - bounds.y_min);
                            }
                        } else {
                            top = f32::NEG_INFINITY;
                            bottom = f32::INFINITY;
                        }
                        if self.emphasis {
                            top = top.min(
                                glyphs.baseline() - run.metrics().ascent - run.font_size() * 0.185,
                            );
                        }
                        let style = glyphs.style();
                        for (decoration, offset, width) in [
                            (
                                &style.underline,
                                run.metrics().underline_offset,
                                run.metrics().underline_size,
                            ),
                            (
                                &style.strikethrough,
                                run.metrics().strikethrough_offset,
                                run.metrics().strikethrough_size,
                            ),
                        ] {
                            if let Some(decoration) = decoration {
                                let y = glyphs.baseline() - decoration.offset.unwrap_or(offset);
                                let width = decoration.size.unwrap_or(width);
                                top = top.min(y - width.abs());
                                bottom = bottom.max(y + width.abs());
                            }
                        }
                    }
                    PositionedLayoutItem::InlineBox(position) => {
                        let annotation = &self.boxes[position.id as usize];
                        for (paragraph, y) in [
                            (&annotation.note, position.y),
                            (&annotation.base, position.y + annotation.note.height()),
                        ] {
                            if let Some(&(first, _)) = paragraph.paint_lines.first() {
                                top = top.min(y + first);
                            }
                            if let Some(&(_, last)) = paragraph.paint_lines.last() {
                                bottom = bottom.max(y + last);
                            }
                        }
                    }
                }
            }
            if let Some(&(_, previous)) = self.paint_lines.last() {
                bottom = bottom.max(previous);
            }
            self.paint_lines.push((top, bottom));
        }
        let mut top = f32::INFINITY;
        for bounds in self.paint_lines.iter_mut().rev() {
            top = top.min(bounds.0);
            bounds.0 = top;
        }
    }

    pub fn visible_lines(
        &self,
        viewport: Option<Rect>,
        transform: Affine,
        outline: Option<TextOutline>,
    ) -> Range<usize> {
        let Some(viewport) = viewport else {
            return 0..self.layout.len();
        };
        if viewport.width() <= 0. || viewport.height() <= 0. {
            return 0..0;
        }
        let inverse = transform.inverse();
        if !inverse.is_finite() {
            return 0..self.layout.len();
        }
        let viewport = inverse.transform_rect_bbox(viewport);
        let padding = outline.map_or(0., |outline| outline.width) + 1.;
        let start = self
            .paint_lines
            .partition_point(|&(_, bottom)| f64::from(bottom) < viewport.y0 - padding);
        let end = self
            .paint_lines
            .partition_point(|&(top, _)| f64::from(top) <= viewport.y1 + padding);
        start..end
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
    #[expect(
        clippy::too_many_arguments,
        reason = "recursive ruby painting shares style, reveal, and viewport inputs"
    )]
    #[cfg(test)]
    pub fn paint(
        &mut self,
        painter: &mut Painter<'_>,
        transform: Affine,
        color: Brush,
        hint: bool,
        visible: Range<usize>,
        outline: Option<TextOutline>,
        viewport: Option<Rect>,
    ) {
        self.paint_faded(
            painter,
            transform,
            color,
            hint,
            visible,
            outline,
            viewport,
            None,
            &[],
            1.,
            None,
        );
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "shared paragraph painting and scalar opacity inputs"
    )]
    pub fn paint_faded(
        &mut self,
        painter: &mut Painter<'_>,
        transform: Affine,
        color: Brush,
        hint: bool,
        visible: Range<usize>,
        outline: Option<TextOutline>,
        viewport: Option<Rect>,
        fade: Option<RevealFade>,
        graphemes: &[usize],
        inherited_alpha: f32,
        brush_override: Option<&Brush>,
    ) {
        self.brushes[0] = self.default_color.clone().unwrap_or_else(|| color.clone());
        let range = self.position(visible.start)..self.position(visible.end);
        let lines = self.visible_lines(viewport, transform, outline);
        if range.start == 0
            && range.end == self.text.len()
            && fade.is_none_or(|fade| fade.alpha(graphemes, self.plain.clone()) == 1.)
            && inherited_alpha == 1.
        {
            render_text_lines(
                painter,
                transform,
                &self.layout,
                &self.brushes,
                hint,
                outline,
                lines.clone(),
                brush_override,
            );
        } else {
            let opacity = |range: Range<usize>| {
                inherited_alpha
                    * fade.map_or(1., |fade| {
                        let start = self.plain_position(range.start).unwrap_or(self.plain.start);
                        let end = self.plain_position(range.end).unwrap_or(self.plain.end);
                        fade.alpha(graphemes, start..end)
                    })
            };
            render_text_faded_lines(
                painter,
                transform,
                &self.layout,
                &self.brushes,
                hint,
                range,
                outline,
                lines.clone(),
                Some(&opacity),
                brush_override,
            );
        }
        if self.emphasis {
            let shown = self.position(visible.start)..self.position(visible.end);
            for index in lines.clone() {
                let line = self.layout.get(index).unwrap();
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
                        let alpha = inherited_alpha
                            * fade.map_or(1., |fade| fade.alpha(graphemes, offset..end));
                        if alpha == 0. {
                            continue;
                        }
                        let color = brush_override.cloned().unwrap_or_else(|| {
                            style.color.map(brush).unwrap_or_else(|| color.clone())
                        });
                        painter
                            .fill(
                                crate::kurbo::Circle::new(
                                    (x as f64, y as f64),
                                    run.font_size() as f64 * 0.045,
                                ),
                                &color,
                            )
                            .transform(transform)
                            .composite(crate::imaging::Composite {
                                alpha,
                                ..Default::default()
                            })
                            .draw();
                    }
                }
            }
        }
        for index in lines {
            let line = self.layout.get(index).unwrap();
            for item in line.items() {
                let PositionedLayoutItem::InlineBox(position) = item else {
                    continue;
                };
                let item = &mut self.boxes[position.id as usize];
                let base_x = position.x + (item.width - item.base.width()) / 2.;
                let base_y = position.y + item.note.height();
                item.base.paint_faded(
                    painter,
                    transform * Affine::translate((base_x as f64, base_y as f64)),
                    color.clone(),
                    hint,
                    visible.clone(),
                    outline,
                    None,
                    fade,
                    graphemes,
                    inherited_alpha,
                    brush_override,
                );
                if visible.start <= item.range.start && item.range.end <= visible.end {
                    let note_x = position.x + (item.width - item.note.width()) / 2.;
                    item.note.paint_faded(
                        painter,
                        transform * Affine::translate((note_x as f64, position.y as f64)),
                        color.clone(),
                        hint,
                        0..usize::MAX,
                        outline,
                        None,
                        None,
                        &[],
                        inherited_alpha
                            * fade.map_or(1., |fade| fade.alpha(graphemes, item.range.clone())),
                        brush_override,
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
        viewport: Option<Rect>,
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
        for index in self.visible_lines(viewport, transform, None) {
            let line = self.layout.get(index).unwrap();
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
                        None,
                    );
                }
            }
        }
    }
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        for index in self.visible_lines(
            Some(Rect::new(
                f64::from(x),
                f64::from(y),
                f64::from(x) + 1.,
                f64::from(y) + 1.,
            )),
            Affine::IDENTITY,
            None,
        ) {
            let line = self.layout.get(index).unwrap();
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
    if let Some(family) = style.font.as_deref() {
        builder.push(
            StyleProperty::FontFamily(reel_text::FontFamily::from(family).into_owned()),
            range.clone(),
        );
    }
    if let Some(height) = style.line_height {
        builder.push(
            StyleProperty::LineHeight(reel_text::LineHeight::Absolute(height)),
            range.clone(),
        );
    }
    if let Some(weight) = style.weight.or_else(|| style.bold.then_some(700.)) {
        builder.push(
            StyleProperty::FontWeight(reel_text::FontWeight::new(weight)),
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

#[cfg(test)]
mod viewport_tests {
    use super::*;
    use crate::{
        imaging::record::{Command, Draw, Scene},
        peniko::Blob,
    };
    use std::time::Instant;

    #[test]
    #[ignore = "requires two configured fonts; CPU font identity and line metrics only"]
    fn rich_fonts_restore_nested_runs_and_absolute_empty_line_metrics() {
        let mut fonts = FontContext {
            collection: reel_text::fontique::Collection::new(
                reel_text::fontique::CollectionOptions {
                    system_fonts: false,
                    ..Default::default()
                },
            ),
            source_cache: Default::default(),
        };
        let mut names = Vec::new();
        let mut ids = Vec::new();
        for key in ["REEL_SESSION_FONT", "REEL_SESSION_SECOND_FONT"] {
            let blob =
                Blob::from(std::fs::read(std::env::var_os(key).expect("configured font")).unwrap());
            ids.push(blob.id());
            let family = fonts.collection.register_fonts(blob, None)[0].0;
            names.push(fonts.collection.family_name(family).unwrap().to_owned());
        }
        let mut styles = StyleSet::new(24.);
        styles.insert(StyleProperty::FontFamily(
            reel_text::FontFamily::from(names[0].as_str()).into_owned(),
        ));
        let source = Text::parse(format!(
            "[font={}][weight=400]A[font={}][weight=700]B[/weight][/font]C[/weight][/font]",
            names[0], names[1]
        ))
        .unwrap();
        let layout = Paragraph::build(
            &mut fonts,
            &mut LayoutContext::new(),
            &source,
            &styles,
            None,
            1.,
            false,
        );
        let runs: Vec<_> = layout
            .lines()
            .flat_map(|line| {
                line.items().filter_map(|item| {
                    if let PositionedLayoutItem::GlyphRun(g) = item {
                        Some(g.run().font().data.id())
                    } else {
                        None
                    }
                })
            })
            .collect();
        assert_eq!(runs, vec![ids[0], ids[1], ids[0]]);
        let source=Text::parse(format!("[font={}][size=36][line_height=46]\n[/line_height][/size][/font][font={}][size=20][line_height=30]\n[/line_height][/size][/font]X",names[0],names[1])).unwrap();
        let layout = Paragraph::build(
            &mut fonts,
            &mut LayoutContext::new(),
            &source,
            &styles,
            None,
            1.,
            false,
        );
        assert!(
            (layout.get(0).unwrap().metrics().line_height - 46.).abs() < 0.01,
            "{:?}",
            layout.get(0).unwrap().metrics()
        );
        assert!((layout.get(1).unwrap().metrics().line_height - 30.).abs() < 0.01);
    }

    fn paragraph(text: &Text) -> Paragraph {
        let bytes =
            std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap();
        let mut fonts = FontContext {
            collection: reel_text::fontique::Collection::new(
                reel_text::fontique::CollectionOptions {
                    system_fonts: false,
                    ..Default::default()
                },
            ),
            source_cache: Default::default(),
        };
        let families = fonts.collection.register_fonts(Blob::from(bytes), None);
        let family = fonts
            .collection
            .family_name(families[0].0)
            .unwrap()
            .to_owned();
        let mut styles = StyleSet::new(24.);
        styles.insert(StyleProperty::FontFamily(
            reel_text::FontFamily::from(family.as_str()).into_owned(),
        ));
        Paragraph::build(
            &mut fonts,
            &mut LayoutContext::new(),
            text,
            &styles,
            Some(840.),
            1.,
            true,
        )
    }

    fn scene(
        paragraph: &mut Paragraph,
        viewport: Option<Rect>,
        transform: Affine,
        reveal: Range<usize>,
    ) -> Scene {
        let mut scene = Scene::new();
        paragraph.paint(
            &mut Painter::new(&mut scene),
            transform,
            Color::BLACK.into(),
            true,
            reveal,
            Some(TextOutline {
                width: 3.,
                color: Color::WHITE,
            }),
            viewport,
        );
        scene
    }

    fn glyph_count(scene: &Scene) -> usize {
        scene
            .commands()
            .iter()
            .map(|command| match command {
                Command::Draw(id) => match scene.draw_op(*id) {
                    Draw::GlyphRun(run) => run.glyphs.len(),
                    _ => 0,
                },
                _ => 0,
            })
            .sum()
    }

    #[test]
    #[ignore = "requires REEL_SESSION_FONT; CPU retained-scene cost and rich-text viewport regression"]
    fn long_paragraph_records_only_visible_lines_without_changing_rich_paint() {
        let source = Text::parse("[b]Speaker:[/b] [u]office ffi[/u] [s]old[/s] [em]marked[/em] [ruby=reading]base[/ruby] [size=36]Large[/size] [color=#ff3399]color[/color]\n".repeat(2000)).unwrap();
        let mut paragraph = paragraph(&source);
        let full = scene(&mut paragraph, None, Affine::IDENTITY, 0..source.len());
        let total = glyph_count(&full);
        let center = paragraph.layout.get(1000).unwrap().metrics().baseline as f64;
        let viewport = Rect::new(0., center - 140., 840., center + 140.);
        let lines = paragraph.visible_lines(Some(viewport), Affine::IDENTITY, None);
        assert!(lines.start > 0 && lines.len() < 30);
        let clipped = scene(
            &mut paragraph,
            Some(viewport),
            Affine::IDENTITY,
            0..source.len(),
        );
        let visible = glyph_count(&clipped);
        assert!(visible > 0 && visible * 30 < total);
        for command in clipped.commands() {
            if let Command::Draw(id) = command {
                let draw = clipped.draw_op(*id);
                assert!(
                    full.commands().iter().any(
                        |command| matches!(command, Command::Draw(id) if full.draw_op(*id) == draw)
                    ),
                    "clipping changed glyph positions, decoration, ruby, or outline"
                );
            }
        }
        let transform = Affine::translate((30., 20.)) * Affine::rotate(0.03) * Affine::scale(1.25);
        for reveal in [0..source.len(), source.len() / 4..source.len() * 3 / 4] {
            let full = scene(&mut paragraph, None, transform, reveal.clone());
            let bounded = scene(
                &mut paragraph,
                Some(Rect::new(-1e9, -1e9, 1e9, 1e9)),
                transform,
                reveal,
            );
            assert_eq!(
                full, bounded,
                "unclipped painting must remain byte-for-byte identical"
            );
        }
        let iterations = 20;
        let started = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(scene(
                &mut paragraph,
                None,
                Affine::IDENTITY,
                0..source.len(),
            ));
        }
        let full_time = started.elapsed();
        let started = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(scene(
                &mut paragraph,
                Some(viewport),
                Affine::IDENTITY,
                0..source.len(),
            ));
        }
        eprintln!(
            "history paint: {} lines, {total} -> {visible} glyphs, {iterations} full recordings {full_time:?}, visible recordings {:?}; line index {} bytes",
            paragraph.layout.len(),
            started.elapsed(),
            paragraph.paint_lines.len() * std::mem::size_of::<(f32, f32)>()
        );
    }
}
