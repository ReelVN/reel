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

use accesskit::{TextDecoration, TextDecorationStyle};
use kurbo::{Affine, Line, Stroke};
use peniko::{Brush, Fill};
use reel_text::{Layout, PositionedLayoutItem, Style};
use smallvec::SmallVec;

use crate::imaging::{PaintSink, Painter, record::Glyph};

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
) {
    for line in layout.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                continue;
            };
            let style = glyph_run.style();
            // We draw underlines under the text, then the strikethrough on top, following:
            // https://drafts.csswg.org/css-text-decor/#painting-order
            if let Some(underline) = &style.underline {
                let underline_brush = &brushes[underline.brush.0];
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
            let glyph_xform = synthesis
                .skew()
                .map(|angle| Affine::skew(angle.to_radians().tan() as f64, 0.0));
            let coords = run.normalized_coords();
            let brush = &brushes[style.brush.0];
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
            painter
                .glyphs(font, brush)
                .hint(hint)
                .transform(transform)
                .glyph_transform(glyph_xform)
                .font_size(font_size)
                .normalized_coords(coords)
                .draw(&peniko::Style::Fill(Fill::NonZero), &glyphs);

            if let Some(strikethrough) = &style.strikethrough {
                let strikethrough_brush = &brushes[strikethrough.brush.0];
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
) {
    render_text_inner(painter, transform, layout, brushes, hint, visible_bytes);
}

fn render_text_inner(
    painter: &mut Painter<'_, impl PaintSink + ?Sized>,
    transform: Affine,
    layout: &Layout<BrushIndex>,
    brushes: &[Brush],
    hint: bool,
    visible_bytes: std::ops::Range<usize>,
) {
    for line in layout.lines() {
        let mut last_run = None;
        let mut glyph_start = 0;
        let mut visibility: SmallVec<[bool; 64]> = SmallVec::new();
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
            let glyph_xform = synthesis
                .skew()
                .map(|angle| Affine::skew(angle.to_radians().tan() as f64, 0.0));
            let coords = run.normalized_coords();
            let brush = &brushes[style.brush.0];
            let cluster_range = run.cluster_range();
            if last_run.as_ref() != Some(&cluster_range) {
                glyph_start = 0;
                last_run = Some(cluster_range);
                visibility.clear();
                let clusters = revealed_clusters(run, &visible_bytes);
                visibility.extend(run.visual_clusters().flat_map(|cluster| {
                    std::iter::repeat_n(
                        clusters[cluster.path().logical_index()],
                        cluster.glyphs().count(),
                    )
                }));
            }
            let mut spans: SmallVec<[(f32, f32); 4]> = SmallVec::new();
            let mut span_start = None;
            let mut glyphs: SmallVec<[Glyph; 16]> = SmallVec::new();
            let mut glyph_count = 0;
            for (glyph, visible) in glyph_run
                .glyphs()
                .zip(visibility[glyph_start..].iter().copied())
            {
                glyph_count += 1;
                let gx = x + glyph.x;
                let gy = y + glyph.y;
                if visible {
                    span_start.get_or_insert(x);
                    glyphs.push(Glyph {
                        id: glyph.id,
                        x: gx,
                        y: gy,
                    });
                } else if let Some(start) = span_start.take() {
                    spans.push((start, x));
                }
                x += glyph.advance;
            }
            glyph_start += glyph_count;
            if let Some(start) = span_start {
                spans.push((start, x));
            }
            // Decorations cover only visible spans and retain the full layout's positions.
            if let Some(underline) = &style.underline {
                let metrics = run.metrics();
                let offset = underline.offset.unwrap_or(metrics.underline_offset);
                let width = underline.size.unwrap_or(metrics.underline_size);
                let y = y - offset + width / 2.;
                for &(start, end) in &spans {
                    painter
                        .stroke(
                            Line::new(
                                (f64::from(start), f64::from(y)),
                                (f64::from(end), f64::from(y)),
                            ),
                            &Stroke::new(width.into()),
                            &brushes[underline.brush.0],
                        )
                        .transform(transform)
                        .draw();
                }
            }
            if !glyphs.is_empty() {
                painter
                    .glyphs(font, brush)
                    .hint(hint)
                    .transform(transform)
                    .glyph_transform(glyph_xform)
                    .font_size(font_size)
                    .normalized_coords(coords)
                    .draw(&peniko::Style::Fill(Fill::NonZero), &glyphs);
            }

            if let Some(strikethrough) = &style.strikethrough {
                let strikethrough_brush = &brushes[strikethrough.brush.0];
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

                for &(start, end) in &spans {
                    let line = Line::new(
                        (f64::from(start), f64::from(y)),
                        (f64::from(end), f64::from(y)),
                    );
                    painter
                        .stroke(line, &Stroke::new(width.into()), strikethrough_brush)
                        .transform(transform)
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

    fn layout(text: &str, styled: bool) -> Layout<BrushIndex> {
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
            );
        } else {
            render_text(&mut painter, Affine::IDENTITY, layout, &brushes, true);
        }
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
