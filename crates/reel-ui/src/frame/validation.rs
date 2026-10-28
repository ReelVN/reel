//! Frame property and operation validation.
use super::*;

pub(super) fn finite(t: Affine) -> bool {
    t.as_coeffs().iter().all(|v| v.is_finite())
}
pub(super) fn valid_color(c: [f32; 4]) -> bool {
    c.iter().all(|v| v.is_finite()) && (0.0..=1.).contains(&c[3])
}
pub(super) fn valid_path_element(element: PathEl) -> bool {
    match element {
        PathEl::MoveTo(p) | PathEl::LineTo(p) => p.is_finite(),
        PathEl::QuadTo(a, b) => a.is_finite() && b.is_finite(),
        PathEl::CurveTo(a, b, c) => a.is_finite() && b.is_finite() && c.is_finite(),
        PathEl::ClosePath => true,
    }
}
pub(super) fn valid_dash(dash: f64) -> bool {
    dash.is_finite() && dash >= 0.
}
pub(super) fn valid_stop(stop: peniko::ColorStop) -> bool {
    stop.offset.is_finite() && valid_color(stop.color.components)
}
pub(super) fn valid_glyph(glyph: record::Glyph) -> bool {
    glyph.x.is_finite() && glyph.y.is_finite()
}
pub(super) fn valid_geometry(g: &Geometry) -> bool {
    match g {
        Geometry::Rect(r) => r.is_finite(),
        Geometry::RoundedRect(r) => r.is_finite(),
        Geometry::Path(_) => true,
    }
}
pub(super) fn valid_stroke(s: &Stroke) -> bool {
    s.width.is_finite()
        && s.width >= 0.
        && s.miter_limit.is_finite()
        && s.miter_limit >= 0.
        && s.dash_offset.is_finite()
}
pub(super) fn valid_clip(c: &Clip) -> bool {
    match c {
        Clip::Fill {
            transform, shape, ..
        } => finite(*transform) && valid_geometry(shape),
        Clip::Stroke {
            transform,
            shape,
            stroke,
        } => finite(*transform) && valid_geometry(shape) && valid_stroke(stroke),
    }
}
pub(super) fn valid_brush(b: &FrameBrush) -> bool {
    match b {
        Brush::Solid(c) => valid_color(c.components),
        Brush::Image(i) => {
            valid_color(i.image.color)
                && i.image.source_rect.is_none_or(|rect| {
                    (rect.width() as f32) > 0.
                        && (rect.height() as f32) > 0.
                        && [
                            rect.x0,
                            rect.y0,
                            rect.x1,
                            rect.y1,
                            rect.width(),
                            rect.height(),
                        ]
                        .iter()
                        .all(|v| (*v as f32).is_finite())
                })
                && i.image.appearance.is_none_or(|appearance| {
                    appearance.adjustment.is_valid()
                        && appearance.gradient.is_none_or(|gradient| {
                            gradient.colors.into_iter().all(valid_color)
                                && finite(gradient.transform)
                        })
                })
                && i.sampler.alpha.is_finite()
                && (0.0..=1.).contains(&i.sampler.alpha)
        }
        Brush::Gradient(g) => match g.kind {
            peniko::GradientKind::Linear(g) => g.start.is_finite() && g.end.is_finite(),
            peniko::GradientKind::Radial(g) => {
                g.start_center.is_finite()
                    && g.end_center.is_finite()
                    && g.start_radius.is_finite()
                    && g.end_radius.is_finite()
                    && g.start_radius >= 0.
                    && g.end_radius >= 0.
            }
            peniko::GradientKind::Sweep(g) => {
                g.center.is_finite() && g.start_angle.is_finite() && g.end_angle.is_finite()
            }
        },
    }
}
pub(super) fn valid_filter(f: Filter) -> bool {
    match f {
        Filter::Flood { color } => valid_color(color.components),
        Filter::Blur {
            std_deviation_x: x,
            std_deviation_y: y,
        } => x.is_finite() && y.is_finite() && x >= 0. && y >= 0.,
        Filter::Offset { dx, dy } => dx.is_finite() && dy.is_finite(),
        Filter::DropShadow {
            dx,
            dy,
            std_deviation_x: x,
            std_deviation_y: y,
            color,
        } => {
            dx.is_finite()
                && dy.is_finite()
                && x.is_finite()
                && y.is_finite()
                && x >= 0.
                && y >= 0.
                && valid_color(color.components)
        }
    }
}
pub(super) fn validate_op(op: &FrameOp) -> Result<(), BuildErrorKind> {
    let valid = match op {
        FrameOp::PushClip(c) => valid_clip(c),
        FrameOp::PushGroup(g) => {
            g.clip.as_ref().is_none_or(valid_clip)
                && g.effect
                    .iter()
                    .chain(g.mask_effect.iter())
                    .all(|e| e.bounds.is_finite() && e.values.iter().all(|v| v.is_finite()))
        }
        FrameOp::Draw(FrameDraw::Fill { shape, brush, .. }) => {
            valid_geometry(shape) && valid_brush(brush)
        }
        FrameOp::Draw(FrameDraw::Stroke {
            shape,
            stroke,
            brush,
            ..
        }) => valid_geometry(shape) && valid_stroke(stroke) && valid_brush(brush),
        FrameOp::Draw(FrameDraw::Sprites(s)) => {
            s.bounds.is_finite()
                && s.bounds.width() > 0.
                && s.bounds.height() > 0.
                && valid_brush(&Brush::Image(s.brush))
        }
        FrameOp::Draw(FrameDraw::GlyphRun(g)) => {
            valid_brush(&g.brush)
                && match &g.style {
                    Style::Stroke(s) => valid_stroke(s),
                    _ => true,
                }
        }
        FrameOp::Draw(FrameDraw::BlurredRoundedRect(d)) => {
            d.rect.is_finite()
                && d.radius.is_finite()
                && d.radius >= 0.
                && d.std_dev.is_finite()
                && d.std_dev >= 0.
                && valid_color(d.color.components)
        }
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err(BuildErrorKind::InvalidRecord)
    }
}
pub(super) fn inflate_aa(ink: InkBounds, padding: f64) -> InkBounds {
    match ink {
        InkBounds::Rect(r) => {
            let bounds = r.inflate(padding, padding);
            if bounds.is_finite() {
                InkBounds::Rect(bounds)
            } else {
                InkBounds::Unbounded
            }
        }
        other => other,
    }
}
pub(super) fn valid_composite(c: Composite) -> bool {
    c.alpha.is_finite() && (0.0..=1.).contains(&c.alpha)
}
