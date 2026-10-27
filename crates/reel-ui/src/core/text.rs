// Copyright 2018 the Xilem Authors and the Druid Authors
// SPDX-License-Identifier: Apache-2.0

//! Support for text display and rendering
//!
//! There are three kinds of text commonly needed:
//!  1) Non interactive text (e.g. a button's label)
//!  2) Selectable text (e.g. a paragraph of content)
//!  3) Editable text (e.g. a search bar)
//!
//! All of these have the same set of global styling options, and can contain rich text

/// A reference counted string slice.
///
/// This is a data-friendly way to represent strings in Masonry. Unlike `String`
/// it cannot be mutated, but unlike `String` it can be cheaply cloned.
pub type ArcStr = std::sync::Arc<str>;

/// The Parley [`Brush`] used within Masonry.
///
/// This enables updating of brush details without performing relayouts;
/// the inner values are indexes into the `brushes` argument to [`render_text()`].
///
/// [`Brush`]: reel_text::Brush
#[derive(Clone, PartialEq, Default, Debug)]
pub struct BrushIndex(pub usize);

/// A style property specialised for use within Masonry.
pub type StyleProperty = reel_text::StyleProperty<'static, BrushIndex>;

/// A set of styles specialised for use within Masonry.
pub type StyleSet = reel_text::StyleSet<BrushIndex>;

/// Scalar opacity wavefront over the text's cached logical grapheme index.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RevealFade {
    /// UTF-8 byte origin; preceding graphemes remain opaque.
    pub from: usize,
    /// Opacity at the origin, before subtracting the per-grapheme step.
    pub progress: f64,
    /// Nonnegative opacity difference between adjacent graphemes.
    pub step: f64,
}
impl RevealFade {
    /// Opacity shared by a complete shaping cluster or source range.
    pub fn alpha(self, graphemes: &[usize], range: std::ops::Range<usize>) -> f32 {
        let origin = graphemes.partition_point(|&offset| offset < self.from);
        let end = graphemes.partition_point(|&offset| offset < range.end);
        if end <= origin {
            return 1.;
        }
        (self.progress - (end - 1 - origin) as f64 * self.step).clamp(0., 1.) as f32
    }
}

/// Width fitting for complete text, independent of its currently revealed prefix.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum TextFit {
    #[default]
    None,
    Shrink {
        min_font_size: f32,
    },
}

/// Outward outline in logical units, drawn beneath the glyph fill.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextOutline {
    pub width: f64,
    pub color: peniko::Color,
}

/// A single shadow of the shaped text, painted before its foreground.
///
/// Offset uses local logical pixels. Color replaces all inline run colors,
/// including ruby and text decorations. Layout and glyph data are shared.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextShadow {
    pub offset: kurbo::Vec2,
    pub color: peniko::Color,
}

/// Coordinates in which a text gradient repeats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextGradientSpace {
    Bounds,
    Glyph,
}

/// Compact two-color foreground paint. Angles use clockwise degrees in the
/// paragraph's logical plane and follow its physical writing-mode transform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextGradient {
    pub from: [u8; 4],
    pub to: [u8; 4],
    pub angle: f64,
    pub space: TextGradientSpace,
}
impl TextGradient {
    pub(crate) fn brush(self, bounds: kurbo::Rect) -> Brush {
        let rgba = |c: [u8; 4]| peniko::Color::from_rgba8(c[0], c[1], c[2], c[3]);
        crate::properties::types::Gradient::new_linear(self.angle.to_radians())
            .with_stops([rgba(self.from), rgba(self.to)])
            .get_peniko_gradient_for_rect(bounds)
            .into()
    }
}

/// Root paragraph bounds expressed in the current ruby paragraph's coordinates.
#[derive(Clone, Copy)]
pub(crate) struct TextGradientPaint {
    pub gradient: TextGradient,
    pub bounds: kurbo::Rect,
}
impl TextGradientPaint {
    pub fn translated(self, offset: Vec2) -> Self {
        Self {
            bounds: self.bounds - offset,
            ..self
        }
    }
}

fn paint_glyph_fill(
    paint: Option<TextGradientPaint>,
    fallback: &Brush,
    glyphs: &[Glyph],
    em_bounds: kurbo::Rect,
    mut draw: impl FnMut(&Brush, &[Glyph]),
) {
    let Some(paint) = paint else {
        draw(fallback, glyphs);
        return;
    };
    if paint.gradient.space == TextGradientSpace::Bounds {
        draw(&paint.gradient.brush(paint.bounds), glyphs);
    } else if let Some(first) = glyphs.first() {
        let bounds = |glyph: &Glyph| em_bounds + Vec2::new(glyph.x as f64, glyph.y as f64);
        // A cross-line gradient shares one brush and one draw for a whole baseline.
        if paint.gradient.angle.to_radians().sin().abs() < 1e-12
            && glyphs.iter().all(|glyph| glyph.y == first.y)
        {
            draw(&paint.gradient.brush(bounds(first)), glyphs);
        } else {
            for glyph in glyphs {
                draw(
                    &paint.gradient.brush(bounds(glyph)),
                    std::slice::from_ref(glyph),
                );
            }
        }
    }
}

// Map the unrotated font's em square back into the paragraph's logical plane.
// The brush follows paragraph axes, while upright outlines compensate for them.
fn glyph_em_bounds(
    writing_mode: reel_text::WritingMode,
    upright: bool,
    em: f32,
    font_ascent: f32,
) -> kurbo::Rect {
    use reel_text::WritingMode::{HorizontalTb, VerticalLr, VerticalRl};
    let em = f64::from(em);
    let ascent = f64::from(font_ascent);
    match (writing_mode, upright) {
        (HorizontalTb, _) | (VerticalRl, false) => kurbo::Rect::new(0., -ascent, em, em - ascent),
        (VerticalRl, true) => kurbo::Rect::new(-ascent, -em, em - ascent, 0.),
        (VerticalLr, true) => kurbo::Rect::new(-ascent, 0., em - ascent, em),
        (VerticalLr, false) => kurbo::Rect::new(0., ascent - em, em, ascent),
    }
}

use accesskit::{TextDecoration, TextDecorationStyle};
use kurbo::{Affine, Line, Stroke, Vec2};
use peniko::{Brush, Fill};
use reel_text::{Layout, PositionedLayoutItem, Style};
use smallvec::SmallVec;

use crate::imaging::{PaintSink, Painter, record::Glyph};

// Paragraph coordinates stay inline/block. Compensate glyph outlines for the
// physical axis map, while retaining font synthesis and the shared paint path.
fn run_glyph_transform(run: &reel_text::Run<'_, BrushIndex>) -> Option<Affine> {
    let synthesis = run
        .synthesis()
        .skew()
        .map(|angle| Affine::skew(angle.to_radians().tan() as f64, 0.0));
    let axes = match run.writing_mode() {
        reel_text::WritingMode::HorizontalTb => return synthesis,
        reel_text::WritingMode::VerticalRl => Affine::new([0., -1., 1., 0., 0., 0.]),
        reel_text::WritingMode::VerticalLr => Affine::new([0., 1., 1., 0., 0., 0.]),
    };
    let orientation = if run.is_upright() {
        // Glyph transforms operate on font y-up outlines before rasterization
        // flips Y. Conjugate the inverse paragraph axes by that flip.
        let flip = Affine::scale_non_uniform(1., -1.);
        flip * axes * flip
    } else {
        axes * Affine::new([0., 1., -1., 0., 0., 0.])
    };
    Some(orientation * synthesis.unwrap_or(Affine::IDENTITY))
}

/// A function that renders laid out glyphs through imaging's [`Painter`].
///
/// The `BrushIndex` values of the runs are indices into `brushes`.
pub fn render_text(
    painter: &mut Painter<'_, impl PaintSink + ?Sized>,
    transform: Affine,
    layout: &Layout<BrushIndex>,
    brushes: &[Brush],
    // TODO: Should this be part of `BrushIndex` (i.e. `brushes`)?
    hint: bool,
    outline: Option<TextOutline>,
) {
    render_text_lines(
        painter,
        transform,
        layout,
        brushes,
        hint,
        outline,
        0..layout.len(),
        None,
        None,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_text_lines(
    painter: &mut Painter<'_, impl PaintSink + ?Sized>,
    transform: Affine,
    layout: &Layout<BrushIndex>,
    brushes: &[Brush],
    hint: bool,
    outline: Option<TextOutline>,
    lines: std::ops::Range<usize>,
    brush_override: Option<&Brush>,
    gradient: Option<TextGradientPaint>,
) {
    for index in lines {
        let line = layout.get(index).unwrap();
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                continue;
            };
            let style = glyph_run.style();
            // We draw underlines under the text, then the strikethrough on top, following:
            // https://drafts.csswg.org/css-text-decor/#painting-order
            if let Some(underline) = &style.underline {
                let underline_brush = brush_override.unwrap_or(&brushes[underline.brush.0]);
                let run_metrics = glyph_run.run().metrics();
                let offset = match underline.offset {
                    Some(offset) => offset,
                    None => run_metrics.underline_offset,
                };
                let width = match underline.size {
                    Some(size) => size,
                    None => run_metrics.underline_size,
                };
                // The `offset` is the distance from the baseline to the top of the underline
                // so we move the line down by half the width
                // Remember that we are using a y-down coordinate system
                // If there's a custom width, because this is an underline, we want the custom
                // width to go down from the default expectation
                let y = glyph_run.baseline() - offset + width / 2.;

                let line = Line::new(
                    (glyph_run.offset() as f64, y as f64),
                    ((glyph_run.offset() + glyph_run.advance()) as f64, y as f64),
                );
                painter
                    .stroke(line, &Stroke::new(width.into()), underline_brush)
                    .transform(transform)
                    .draw();
            }
            let mut x = glyph_run.offset();
            let y = glyph_run.baseline();
            let run = glyph_run.run();
            let font = run.font();
            let font_size = run.font_size();
            let synthesis = run.synthesis();
            let font_embolden = if synthesis.embolden() {
                // The total stroke increase is one twenty-fourth of this run's
                // em, before its glyph transform and raster scale.
                Vec2::new(1., 1.) * (f64::from(font_size) / 24.)
            } else {
                Vec2::ZERO
            };
            let glyph_xform = run_glyph_transform(run);
            let coords = run.normalized_coords();
            let brush = brush_override.unwrap_or(&brushes[style.brush.0]);
            let gradient = gradient.filter(|_| brush_override.is_none() && style.brush.0 == 0);
            let glyphs: SmallVec<[Glyph; 16]> = glyph_run
                .glyphs()
                .map(|glyph| {
                    let gx = x + glyph.x;
                    let gy = y + glyph.y;
                    x += glyph.advance;
                    Glyph {
                        id: glyph.id,
                        x: gx,
                        y: gy,
                    }
                })
                .collect();
            if let Some(outline) = outline.filter(|outline| outline.width > 0.) {
                painter
                    .glyphs(font, &Brush::Solid(outline.color))
                    .hint(hint)
                    .transform(transform)
                    .glyph_transform(glyph_xform)
                    .font_embolden(font_embolden)
                    .font_size(font_size)
                    .normalized_coords(coords)
                    .draw(
                        &peniko::Style::Stroke(Stroke::new(outline.width * 2.)),
                        &glyphs,
                    );
            }
            paint_glyph_fill(
                gradient,
                brush,
                &glyphs,
                glyph_em_bounds(
                    run.writing_mode(),
                    run.is_upright(),
                    font_size,
                    run.metrics().font_ascent,
                ),
                |brush, glyphs| {
                    painter
                        .glyphs(font, brush)
                        .hint(hint)
                        .transform(transform)
                        .glyph_transform(glyph_xform)
                        .font_embolden(font_embolden)
                        .font_size(font_size)
                        .normalized_coords(coords)
                        .draw(&peniko::Style::Fill(Fill::NonZero), glyphs);
                },
            );

            if let Some(strikethrough) = &style.strikethrough {
                let strikethrough_brush = brush_override.unwrap_or(&brushes[strikethrough.brush.0]);
                let run_metrics = glyph_run.run().metrics();
                let offset = match strikethrough.offset {
                    Some(offset) => offset,
                    None => run_metrics.strikethrough_offset,
                };
                let width = match strikethrough.size {
                    Some(size) => size,
                    None => run_metrics.strikethrough_size,
                };
                // The `offset` is the distance from the baseline to the *top* of the strikethrough
                // so we calculate the middle y-position of the strikethrough based on the font's
                // standard strikethrough width.
                // Remember that we are using a y-down coordinate system
                let y = glyph_run.baseline() - offset + run_metrics.strikethrough_size / 2.;

                let line = Line::new(
                    (glyph_run.offset() as f64, y as f64),
                    ((glyph_run.offset() + glyph_run.advance()) as f64, y as f64),
                );
                painter
                    .stroke(line, &Stroke::new(width.into()), strikethrough_brush)
                    .transform(transform)
                    .draw();
            }
        }
    }
}

/// Paint complete shaping clusters inside a logical UTF-8 byte range.
///
/// The supplied layout always contains the full text. Hidden glyphs retain their
/// advances, so reveal changes never reshape or move the remaining text. A
/// ligature is painted only when all of its component clusters are visible.
pub fn render_text_revealed(
    painter: &mut Painter<'_, impl PaintSink + ?Sized>,
    transform: Affine,
    layout: &Layout<BrushIndex>,
    brushes: &[Brush],
    hint: bool,
    visible_bytes: std::ops::Range<usize>,
    outline: Option<TextOutline>,
) {
    render_text_revealed_lines(
        painter,
        transform,
        layout,
        brushes,
        hint,
        visible_bytes,
        outline,
        0..layout.len(),
    );
}

#[expect(
    clippy::too_many_arguments,
    reason = "painting inputs plus independent byte and line visibility"
)]
pub(crate) fn render_text_revealed_lines(
    painter: &mut Painter<'_, impl PaintSink + ?Sized>,
    transform: Affine,
    layout: &Layout<BrushIndex>,
    brushes: &[Brush],
    hint: bool,
    visible_bytes: std::ops::Range<usize>,
    outline: Option<TextOutline>,
    lines: std::ops::Range<usize>,
) {
    render_text_faded_lines(
        painter,
        transform,
        layout,
        brushes,
        hint,
        visible_bytes,
        outline,
        lines,
        None,
        None,
        None,
    );
}
#[expect(
    clippy::too_many_arguments,
    reason = "shared shaping inputs and optional cluster opacity"
)]
pub(crate) fn render_text_faded_lines(
    painter: &mut Painter<'_, impl PaintSink + ?Sized>,
    transform: Affine,
    layout: &Layout<BrushIndex>,
    brushes: &[Brush],
    hint: bool,
    visible_bytes: std::ops::Range<usize>,
    outline: Option<TextOutline>,
    lines: std::ops::Range<usize>,
    alpha: Option<&dyn Fn(std::ops::Range<usize>) -> f32>,
    brush_override: Option<&Brush>,
    gradient: Option<TextGradientPaint>,
) {
    for index in lines {
        let line = layout.get(index).unwrap();
        let mut last_run = None;
        let mut glyph_start = 0;
        let mut visibility: SmallVec<[f32; 64]> = SmallVec::new();
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                continue;
            };
            let style = glyph_run.style();
            let mut x = glyph_run.offset();
            let y = glyph_run.baseline();
            let run = glyph_run.run();
            let font = run.font();
            let font_size = run.font_size();
            let synthesis = run.synthesis();
            let font_embolden = if synthesis.embolden() {
                // The total stroke increase is one twenty-fourth of this run's
                // em, before its glyph transform and raster scale.
                Vec2::new(1., 1.) * (f64::from(font_size) / 24.)
            } else {
                Vec2::ZERO
            };
            let glyph_xform = run_glyph_transform(run);
            let coords = run.normalized_coords();
            let brush = brush_override.unwrap_or(&brushes[style.brush.0]);
            let gradient = gradient.filter(|_| brush_override.is_none() && style.brush.0 == 0);
            let cluster_range = run.cluster_range();
            if last_run.as_ref() != Some(&cluster_range) {
                glyph_start = 0;
                last_run = Some(cluster_range);
                visibility.clear();
                let clusters = faded_clusters(run, &visible_bytes, alpha);
                visibility.extend(run.visual_clusters().flat_map(|cluster| {
                    let opacity = clusters[cluster.path().logical_index()];
                    std::iter::repeat_n(opacity, cluster.glyphs().count())
                }));
            }
            let mut spans: SmallVec<[(f32, f32, f32); 4]> = SmallVec::new();
            let mut span_start = None;
            let mut glyphs: SmallVec<[Glyph; 16]> = SmallVec::new();
            let mut glyph_count = 0;
            let mut groups: SmallVec<[(f32, std::ops::Range<usize>); 4]> = SmallVec::new();
            for (glyph, visible) in glyph_run
                .glyphs()
                .zip(visibility[glyph_start..].iter().copied())
            {
                glyph_count += 1;
                let gx = x + glyph.x;
                let gy = y + glyph.y;
                if visible > 0. {
                    if span_start.is_some_and(|(_, a)| a != visible) {
                        let (start, a) = span_start.take().unwrap();
                        spans.push((start, x, a));
                    }
                    span_start.get_or_insert((x, visible));
                    if groups.last().is_none_or(|(a, _)| *a != visible) {
                        groups.push((visible, glyphs.len()..glyphs.len()));
                    }
                    groups.last_mut().unwrap().1.end += 1;
                    glyphs.push(Glyph {
                        id: glyph.id,
                        x: gx,
                        y: gy,
                    });
                } else if let Some((start, a)) = span_start.take() {
                    spans.push((start, x, a));
                }
                x += glyph.advance;
            }
            glyph_start += glyph_count;
            if let Some((start, a)) = span_start {
                spans.push((start, x, a));
            }
            // Decorations cover only visible spans and retain the full layout's positions.
            if let Some(underline) = &style.underline {
                let metrics = run.metrics();
                let offset = underline.offset.unwrap_or(metrics.underline_offset);
                let width = underline.size.unwrap_or(metrics.underline_size);
                let y = y - offset + width / 2.;
                for &(start, end, alpha) in &spans {
                    painter
                        .stroke(
                            Line::new(
                                (f64::from(start), f64::from(y)),
                                (f64::from(end), f64::from(y)),
                            ),
                            &Stroke::new(width.into()),
                            brush_override.unwrap_or(&brushes[underline.brush.0]),
                        )
                        .transform(transform)
                        .composite(crate::imaging::Composite {
                            alpha,
                            ..Default::default()
                        })
                        .draw();
                }
            }
            for (alpha, range) in groups {
                let glyphs = &glyphs[range];
                if let Some(outline) = outline.filter(|outline| outline.width > 0.) {
                    painter
                        .glyphs(font, &Brush::Solid(outline.color))
                        .composite(crate::imaging::Composite {
                            alpha,
                            ..Default::default()
                        })
                        .hint(hint)
                        .transform(transform)
                        .glyph_transform(glyph_xform)
                        .font_embolden(font_embolden)
                        .font_size(font_size)
                        .normalized_coords(coords)
                        .draw(
                            &peniko::Style::Stroke(Stroke::new(outline.width * 2.)),
                            glyphs,
                        );
                }
                paint_glyph_fill(
                    gradient,
                    brush,
                    glyphs,
                    glyph_em_bounds(
                        run.writing_mode(),
                        run.is_upright(),
                        font_size,
                        run.metrics().font_ascent,
                    ),
                    |brush, glyphs| {
                        painter
                            .glyphs(font, brush)
                            .composite(crate::imaging::Composite {
                                alpha,
                                ..Default::default()
                            })
                            .hint(hint)
                            .transform(transform)
                            .glyph_transform(glyph_xform)
                            .font_embolden(font_embolden)
                            .font_size(font_size)
                            .normalized_coords(coords)
                            .draw(&peniko::Style::Fill(Fill::NonZero), glyphs);
                    },
                );
            }

            if let Some(strikethrough) = &style.strikethrough {
                let strikethrough_brush = brush_override.unwrap_or(&brushes[strikethrough.brush.0]);
                let run_metrics = glyph_run.run().metrics();
                let offset = match strikethrough.offset {
                    Some(offset) => offset,
                    None => run_metrics.strikethrough_offset,
                };
                let width = match strikethrough.size {
                    Some(size) => size,
                    None => run_metrics.strikethrough_size,
                };
                // The `offset` is the distance from the baseline to the *top* of the strikethrough
                // so we calculate the middle y-position of the strikethrough based on the font's
                // standard strikethrough width.
                // Remember that we are using a y-down coordinate system
                let y = glyph_run.baseline() - offset + run_metrics.strikethrough_size / 2.;

                for &(start, end, alpha) in &spans {
                    let line = Line::new(
                        (f64::from(start), f64::from(y)),
                        (f64::from(end), f64::from(y)),
                    );
                    painter
                        .stroke(line, &Stroke::new(width.into()), strikethrough_brush)
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
}

/// Linear visibility map indexed by logical cluster. Parley's ligature start
/// precedes its continuations in visual order, including RTL runs where those
/// continuations precede the start in logical order. Group in visual order, then
/// restore logical indexing; no prefix scan repeats for a ligature or style run.
fn faded_clusters(
    run: &reel_text::Run<'_, BrushIndex>,
    visible: &std::ops::Range<usize>,
    alpha: Option<&dyn Fn(std::ops::Range<usize>) -> f32>,
) -> SmallVec<[f32; 64]> {
    let mut masks = SmallVec::new();
    let mut clusters = run.visual_clusters().peekable();
    while let Some(cluster) = clusters.next() {
        let mut range = cluster.text_range();
        let mut count = 1;
        if cluster.is_ligature_start() {
            while clusters
                .peek()
                .is_some_and(|next| next.is_ligature_continuation())
            {
                let next = clusters.next().unwrap().text_range();
                range.start = range.start.min(next.start);
                range.end = range.end.max(next.end);
                count += 1;
            }
        }
        let opacity = if visible.start <= range.start && range.end <= visible.end {
            alpha.map_or(1., |f| f(range))
        } else {
            0.
        };
        masks.extend(std::iter::repeat_n(opacity, count));
    }
    if run.is_rtl() {
        masks.reverse();
    }
    masks
}

pub(crate) fn revealed_clusters(
    run: &reel_text::Run<'_, BrushIndex>,
    visible: &std::ops::Range<usize>,
) -> SmallVec<[bool; 64]> {
    let mut masks = SmallVec::new();
    let mut clusters = run.visual_clusters().peekable();
    while let Some(cluster) = clusters.next() {
        let mut range = cluster.text_range();
        let mut count = 1;
        if cluster.is_ligature_start() {
            while clusters
                .peek()
                .is_some_and(|next| next.is_ligature_continuation())
            {
                let next = clusters.next().expect("peeked ligature component");
                let next_range = next.text_range();
                range.start = range.start.min(next_range.start);
                range.end = range.end.max(next_range.end);
                count += 1;
            }
        }
        let shown = visible.start <= range.start && range.end <= visible.end;
        masks.extend(std::iter::repeat_n(shown, count));
    }
    if run.is_rtl() {
        masks.reverse();
    }
    masks
}

fn to_accesskit_color(brush: &Brush) -> Option<accesskit::Color> {
    if let Brush::Solid(color) = brush {
        let rgba = color.to_rgba8();
        Some(accesskit::Color {
            red: rgba.r,
            green: rgba.g,
            blue: rgba.b,
            alpha: rgba.a,
        })
    } else {
        None
    }
}

/// Sets AccessKit text properties from the brush(es) for the given style.
///
/// The `BrushIndex` values of the runs are indices into `brushes`.
pub fn set_accesskit_brush_properties(
    node: &mut accesskit::Node,
    style: &Style<BrushIndex>,
    brushes: &[Brush],
) {
    if let Some(color) = to_accesskit_color(&brushes[style.brush.0]) {
        node.set_foreground_color(color);
    }
    if let Some(deco) = &style.underline
        && let Some(color) = to_accesskit_color(&brushes[deco.brush.0])
    {
        node.set_underline(TextDecoration {
            style: TextDecorationStyle::Solid,
            color,
        });
    }
    if let Some(deco) = &style.strikethrough
        && let Some(color) = to_accesskit_color(&brushes[deco.brush.0])
    {
        node.set_strikethrough(TextDecoration {
            style: TextDecorationStyle::Solid,
            color,
        });
    }
}

#[cfg(test)]
mod reveal_tests {
    use super::*;
    use crate::imaging::record::{Command, Draw, Scene};
    use crate::peniko::{Blob, Color};
    use crate::reel_text::{FontContext, LayoutContext};

    #[test]
    #[ignore = "requires REEL_SESSION_FONT; shaping clusters and CPU draw records only"]
    fn reveal_fade_keeps_ligatures_atomic_in_logical_rtl_order() {
        let source = "لا لا office ffi e\u{301}";
        let layout = layout(source, false);
        let calls = std::cell::RefCell::new(Vec::new());
        let opacity = |range: std::ops::Range<usize>| {
            calls.borrow_mut().push(range.clone());
            (100 - range.end) as f32 / 100.
        };
        let mut ligatures = 0;
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(glyphs) = item else {
                    continue;
                };
                let run = glyphs.run();
                let masks = faded_clusters(run, &(0..source.len()), Some(&opacity));
                assert_eq!(masks.len(), run.visual_clusters().count());
                let mut clusters = run.visual_clusters().peekable();
                while let Some(cluster) = clusters.next() {
                    if !cluster.is_ligature_start() {
                        continue;
                    }
                    let mut bytes = cluster.text_range();
                    let mut components = vec![cluster.path().logical_index()];
                    while clusters
                        .peek()
                        .is_some_and(|c| c.is_ligature_continuation())
                    {
                        let c = clusters.next().unwrap();
                        let range = c.text_range();
                        bytes.start = bytes.start.min(range.start);
                        bytes.end = bytes.end.max(range.end);
                        components.push(c.path().logical_index());
                    }
                    assert!(components.len() > 1);
                    let expected = (100 - bytes.end) as f32 / 100.;
                    for component in components {
                        assert_eq!(masks[component], expected);
                    }
                    assert!(calls.borrow().contains(&bytes));
                    ligatures += 1;
                }
            }
        }
        assert!(
            ligatures > 0,
            "test font must shape at least one mandatory Arabic ligature"
        );
    }

    fn layout(text: &str, styled: bool) -> Layout<BrushIndex> {
        weighted_layout(text, styled, 400.)
    }
    fn weighted_layout(text: &str, styled: bool, weight: f32) -> Layout<BrushIndex> {
        let bytes = std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect(
            "set REEL_SESSION_FONT to a real font with Latin ligatures and Hebrew/Arabic glyphs",
        ))
        .unwrap();
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
        let mut context = LayoutContext::new();
        let mut builder = context.ranged_builder(&mut fonts, text, 1., false);
        builder.push_default(reel_text::StyleProperty::FontFamily(family.as_str().into()));
        builder.push_default(reel_text::StyleProperty::FontSize(24.));
        builder.push_default(reel_text::StyleProperty::FontWeight(
            reel_text::FontWeight::new(weight),
        ));
        if styled {
            builder.push_default(reel_text::StyleProperty::Underline(true));
            builder.push_default(reel_text::StyleProperty::Strikethrough(true));
            for (index, (byte, ch)) in text.char_indices().enumerate() {
                builder.push(
                    reel_text::StyleProperty::Brush(BrushIndex(index % 2)),
                    byte..byte + ch.len_utf8(),
                );
            }
        }
        let mut layout = builder.build(text);
        layout.break_all_lines(Some(230.));
        layout
    }
    fn painted(layout: &Layout<BrushIndex>, range: Option<std::ops::Range<usize>>) -> Scene {
        let mut scene = Scene::new();
        let mut painter = Painter::new(&mut scene);
        let brushes = [Color::BLACK.into(), Color::WHITE.into()];
        if let Some(range) = range {
            render_text_revealed(
                &mut painter,
                Affine::IDENTITY,
                layout,
                &brushes,
                true,
                range,
                None,
            );
        } else {
            render_text(&mut painter, Affine::IDENTITY, layout, &brushes, true, None);
        }
        scene
    }
    fn outlined(layout: &Layout<BrushIndex>, range: std::ops::Range<usize>) -> Scene {
        let mut scene = Scene::new();
        let mut painter = Painter::new(&mut scene);
        render_text_revealed(
            &mut painter,
            Affine::IDENTITY,
            layout,
            &[Color::BLACK.into(), Color::WHITE.into()],
            true,
            range,
            Some(TextOutline {
                width: 2.,
                color: Color::from_rgb8(255, 170, 255),
            }),
        );
        scene
    }
    fn glyphs(scene: &Scene) -> Vec<(Glyph, Brush)> {
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
            .flat_map(|run| run.glyphs.iter().map(|glyph| (*glyph, run.brush.clone())))
            .collect()
    }

    #[test]
    #[ignore = "requires REEL_SESSION_FONT containing a single Regular face"]
    fn synthetic_bold_preserves_shaping_and_reveal_in_fill_and_outline() {
        fn runs(scene: &Scene) -> Vec<&crate::imaging::record::GlyphRun> {
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
                .collect()
        }
        let text = "Bold office";
        let regular = weighted_layout(text, false, 400.);
        let bold = weighted_layout(text, false, 700.);
        let regular_scene = painted(&regular, None);
        let bold_scene = painted(&bold, None);
        assert_eq!(
            glyphs(&regular_scene),
            glyphs(&bold_scene),
            "synthetic weight keeps shaping positions"
        );
        assert!(
            runs(&regular_scene)
                .iter()
                .all(|run| run.font_embolden == Vec2::ZERO)
        );
        assert!(!runs(&bold_scene).is_empty());
        assert!(
            runs(&bold_scene)
                .iter()
                .all(|run| run.font_embolden.x > 0. && run.font_embolden.y > 0.)
        );
        for end in [4, text.len()] {
            let revealed = painted(&bold, Some(0..end));
            let outlined = outlined(&bold, 0..end);
            let reveal_runs = runs(&revealed);
            let outlined_runs = runs(&outlined);
            assert!(glyphs(&revealed).len() <= glyphs(&bold_scene).len());
            assert_eq!(outlined_runs.len(), reveal_runs.len() * 2);
            for (plain, pair) in reveal_runs
                .iter()
                .zip(outlined_runs.as_chunks::<2>().0.iter())
            {
                for drawn in pair {
                    assert_eq!(drawn.font_embolden, plain.font_embolden);
                    assert!(drawn.font_embolden.x > 0.);
                    assert_eq!(drawn.glyphs, plain.glyphs);
                }
            }
            if end == text.len() {
                assert_eq!(glyphs(&revealed), glyphs(&bold_scene));
            } else {
                assert!(glyphs(&revealed).len() < glyphs(&bold_scene).len());
            }
        }
    }

    #[test]
    #[ignore = "requires REEL_SESSION_FONT containing a single Bold face"]
    fn real_bold_face_does_not_receive_synthetic_weight() {
        let layout = weighted_layout("Real bold", false, 700.);
        let mut count = 0;
        for scene in [painted(&layout, None), outlined(&layout, 0..4)] {
            for command in scene.commands() {
                if let Command::Draw(id) = command
                    && let Draw::GlyphRun(run) = scene.draw_op(*id)
                {
                    assert_eq!(run.font_embolden, Vec2::ZERO);
                    count += 1;
                }
            }
        }
        assert!(count > 0);
    }

    #[test]
    #[ignore = "requires REEL_SESSION_FONT with real Latin ligatures and Hebrew glyphs"]
    fn complete_clusters_and_ligatures_keep_original_glyph_positions_including_rtl_and_styles() {
        for text in [
            "e\u{301}x",
            "ffi",
            "office ffi fi",
            "אבג office ffi",
            "styled words wrap across several lines",
        ] {
            for styled in [false, true] {
                let layout = layout(text, styled);
                let original = glyphs(&painted(&layout, None));
                // Each outline reuses precisely the fill glyph positions. A
                // partially revealed ligature cannot leave a hidden outline.
                for end in text.char_indices().map(|(i, _)| i).chain([text.len()]) {
                    let ordinary = glyphs(&painted(&layout, Some(0..end)));
                    let stroked = outlined(&layout, 0..end);
                    let drawn = glyphs(&stroked);
                    let fill: Vec<_> = drawn
                        .iter()
                        .filter(|(_, brush)| {
                            *brush != Brush::Solid(Color::from_rgb8(255, 170, 255))
                        })
                        .cloned()
                        .collect();
                    assert_eq!(fill, ordinary);
                    assert_eq!(drawn.len(), ordinary.len() * 2);
                    for command in stroked.commands() {
                        if let Command::Draw(id) = command
                            && let Draw::GlyphRun(run) = stroked.draw_op(*id)
                            && run.brush == Brush::Solid(Color::from_rgb8(255, 170, 255))
                        {
                            assert!(
                                matches!(&run.style, peniko::Style::Stroke(stroke) if stroke.width == 4.)
                            );
                        }
                    }
                }
                assert!(!original.is_empty(), "real font must shape this case");
                assert_eq!(
                    glyphs(&painted(&layout, Some(0..text.len()))),
                    original,
                    "all-visible paint matches existing text renderer: {text} styled={styled}"
                );
                assert!(glyphs(&painted(&layout, Some(0..0))).is_empty());
                for boundary in text.char_indices().map(|(byte, _)| byte) {
                    let partial = glyphs(&painted(&layout, Some(0..boundary)));
                    assert!(
                        partial.iter().all(|glyph| original.contains(glyph)),
                        "partial reveal moved or substituted a glyph: {text} at {boundary}"
                    );
                }
            }
        }
        let combining = layout("e\u{301}x", false);
        assert!(
            glyphs(&painted(&combining, Some(0..1))).is_empty(),
            "an incomplete combining cluster is hidden"
        );
        assert!(!glyphs(&painted(&combining, Some(0..3))).is_empty());
        let ligature = layout("ffi", false);
        assert!(
            ligature
                .lines()
                .flat_map(|line| line.runs())
                .any(|run| run.clusters().any(|cluster| cluster.is_ligature_start())),
            "test font must have a real ffi ligature"
        );
        assert!(glyphs(&painted(&ligature, Some(0..1))).is_empty());
        assert!(glyphs(&painted(&ligature, Some(0..2))).is_empty());
        assert!(!glyphs(&painted(&ligature, Some(0..3))).is_empty());
    }

    #[test]
    #[ignore = "requires REEL_SESSION_FONT with real Latin and Arabic ligature shaping"]
    fn arbitrary_ranges_never_paint_a_partial_ltr_or_rtl_ligature() {
        for (text, rtl, continuations) in [
            ("fi", false, 1),
            ("ffi", false, 2),
            ("لا", true, 1),
            ("ل\u{64e}ا", true, 2),
            ("e\u{301}", false, 1),
        ] {
            for styled in [false, true] {
                let layout = layout(text, styled);
                let runs: Vec<_> = layout.lines().flat_map(|line| line.runs()).collect();
                assert_eq!(runs.len(), 1, "one shaping run: {text} styled={styled}");
                let run = &runs[0];
                assert_eq!(run.is_rtl(), rtl);
                assert_eq!(
                    run.clusters().filter(|c| c.is_ligature_start()).count(),
                    1,
                    "real font must shape one atomic group: {text} styled={styled}"
                );
                assert_eq!(
                    run.clusters()
                        .filter(|c| c.is_ligature_continuation())
                        .count(),
                    continuations,
                    "real single/multiple continuation fixture: {text} styled={styled}"
                );
                let original = glyphs(&painted(&layout, None));
                assert!(!original.is_empty());
                let boundaries: Vec<_> = text
                    .char_indices()
                    .map(|(byte, _)| byte)
                    .chain(std::iter::once(text.len()))
                    .collect();
                for (index, &start) in boundaries.iter().enumerate() {
                    for &end in &boundaries[index..] {
                        let scene = painted(&layout, Some(start..end));
                        if start == 0 && end == text.len() {
                            assert_eq!(glyphs(&scene), original);
                        } else {
                            assert!(
                                glyphs(&scene).is_empty(),
                                "partial atomic group leaked glyphs: {text} styled={styled} range={start}..{end}"
                            );
                            assert!(
                                scene
                                    .commands()
                                    .iter()
                                    .all(|c| !matches!(c, Command::Draw(_))),
                                "hidden group leaked decorations: {text} styled={styled} range={start}..{end}"
                            );
                        }
                    }
                }
                eprintln!(
                    "arbitrary-range atomic font case: {text:?}, rtl={rtl}, continuations={continuations}, styled={styled}, glyphs={}",
                    original.len()
                );
            }
        }
    }

    #[test]
    #[ignore = "requires REEL_SESSION_FONT with real Latin and Arabic ligature shaping"]
    fn mixed_direction_ranges_paint_only_complete_units_at_original_positions_and_styles() {
        // These units have independently known atomic boundaries. Selection of
        // any strict subrange of fi/ffi/lam-alef/combining clusters must hide that
        // entire unit; spaces and x remain independently paintable.
        let units = [
            "fi",
            " ",
            "ffi",
            " ",
            "لا",
            " ",
            "ل\u{64e}ا",
            " ",
            "e\u{301}",
            "x",
        ];
        let text: String = units.concat();
        for styled in [false, true] {
            let layout = layout(&text, styled);
            let original = glyphs(&painted(&layout, None));
            let mut offset = 0;
            let unit_glyphs: Vec<_> = units
                .iter()
                .map(|unit| {
                    let range = offset..offset + unit.len();
                    offset = range.end;
                    let glyphs = glyphs(&painted(&layout, Some(range.clone())));
                    assert!(!glyphs.is_empty(), "real font must paint unit {unit:?}");
                    (range, glyphs)
                })
                .collect();
            assert_eq!(
                unit_glyphs.iter().map(|(_, g)| g.len()).sum::<usize>(),
                original.len()
            );
            let boundaries: Vec<_> = text
                .char_indices()
                .map(|(byte, _)| byte)
                .chain(std::iter::once(text.len()))
                .collect();
            for (index, &start) in boundaries.iter().enumerate() {
                for &end in &boundaries[index..] {
                    // Filter the original visual sequence; the expected glyphs
                    // retain the original position and brush, including styles
                    // that split a shaping run into multiple painted glyph runs.
                    let expected: Vec<_> = original
                        .iter()
                        .filter(|glyph| {
                            unit_glyphs.iter().any(|(range, glyphs)| {
                                start <= range.start && range.end <= end && glyphs.contains(glyph)
                            })
                        })
                        .cloned()
                        .collect();
                    let scene = painted(&layout, Some(start..end));
                    assert_eq!(
                        glyphs(&scene),
                        expected,
                        "mixed non-prefix range changed selection/position/style: styled={styled} range={start}..{end}"
                    );
                    if expected.is_empty() {
                        assert!(
                            scene
                                .commands()
                                .iter()
                                .all(|c| !matches!(c, Command::Draw(_)))
                        );
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "requires REEL_SESSION_FONT with real ligature shaping"]
    fn many_ligatures_use_one_visibility_entry_per_logical_cluster() {
        for pattern in ["fi ffi ", "fi ffi لا ل\u{64e}ا "] {
            let text = pattern.repeat(8192);
            let layout = layout(&text, false);
            for visible in [
                0..pattern.len() * 4096,
                pattern.len() * 2048..pattern.len() * 6144,
            ] {
                let mut cluster_count = 0;
                let mut mask_count = 0;
                let mut rtl_clusters = 0;
                for line in layout.lines() {
                    for run in line.runs() {
                        cluster_count += run.len();
                        if run.is_rtl() {
                            rtl_clusters += run.len();
                        }
                        mask_count += revealed_clusters(&run, &visible).len();
                    }
                }
                assert_eq!(cluster_count, mask_count);
                let partial = glyphs(&painted(&layout, Some(visible.clone())));
                assert!(!partial.is_empty());
                eprintln!(
                    "linear reveal projection: {cluster_count} clusters ({rtl_clusters} RTL), {mask_count} masks, {} painted glyphs, range={visible:?}",
                    partial.len()
                );
            }
        }
    }
}

#[cfg(test)]
mod gradient_tests {
    use super::*;

    #[test]
    fn text_gradient_batches_baselines_and_repeats_oblique_em_cells() {
        let glyphs = [
            Glyph {
                id: 1,
                x: 0.,
                y: 20.,
            },
            Glyph {
                id: 2,
                x: 24.,
                y: 20.,
            },
        ];
        let mut paint = TextGradientPaint {
            gradient: TextGradient {
                from: [255; 4],
                to: [128, 128, 128, 255],
                angle: 180.,
                space: TextGradientSpace::Glyph,
            },
            bounds: kurbo::Rect::new(0., 0., 48., 24.),
        };
        let fallback = Brush::Solid(peniko::Color::BLACK);
        let mut counts = Vec::new();
        let em_bounds = glyph_em_bounds(reel_text::WritingMode::HorizontalTb, false, 24., 20.);
        paint_glyph_fill(
            Some(paint),
            &fallback,
            &glyphs,
            em_bounds,
            |brush, group| {
                assert!(matches!(brush, Brush::Gradient(_)));
                counts.push(group.len());
            },
        );
        assert_eq!(counts, [2]);
        paint.gradient.angle = 90.;
        let mut brushes = Vec::new();
        paint_glyph_fill(
            Some(paint),
            &fallback,
            &glyphs,
            em_bounds,
            |brush, group| {
                assert_eq!(group.len(), 1);
                brushes.push(brush.clone());
            },
        );
        assert_ne!(brushes[0], brushes[1]);
        paint.gradient.space = TextGradientSpace::Bounds;
        counts.clear();
        paint_glyph_fill(Some(paint), &fallback, &glyphs, em_bounds, |_, group| {
            counts.push(group.len())
        });
        assert_eq!(counts, [2]);
    }

    #[test]
    fn glyph_gradient_em_bounds_follow_physical_font_orientation() {
        use reel_text::WritingMode::{HorizontalTb, VerticalLr, VerticalRl};

        let font_em = kurbo::Rect::new(0., -20., 24., 4.);
        let clockwise = Affine::new([0., 1., -1., 0., 0., 0.]);
        let origin = Vec2::new(31., 47.);
        for (mode, upright, axes) in [
            (HorizontalTb, false, Affine::IDENTITY),
            (VerticalRl, true, clockwise),
            (VerticalLr, true, Affine::new([0., 1., 1., 0., 0., 0.])),
            (VerticalRl, false, clockwise),
            (VerticalLr, false, Affine::new([0., 1., 1., 0., 0., 0.])),
        ] {
            let bounds = glyph_em_bounds(mode, upright, 24., 20.);
            let physical_origin = (axes * origin.to_point()).to_vec2();
            let physical_em = if mode == HorizontalTb || upright {
                font_em
            } else {
                clockwise.transform_rect_bbox(font_em)
            };
            assert_eq!(
                axes.transform_rect_bbox(bounds + origin),
                physical_em + physical_origin,
                "{mode:?}, upright={upright}",
            );
        }
    }
}
