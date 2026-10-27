//! Command field copying and draw ink bounds.
use super::*;

pub(super) fn cmd_id(scene: &Scene, pos: usize) -> &Command {
    &scene.commands()[pos]
}
pub(super) fn source_geometry<'a>(scene: &'a Scene, c: &Command) -> Option<&'a Geometry> {
    match c {
        Command::PushClip(id) => Some(match scene.clip(*id) {
            Clip::Fill { shape, .. } | Clip::Stroke { shape, .. } => shape,
        }),
        Command::PushGroup(id) => scene.group(*id).clip.as_ref().map(|c| match c {
            Clip::Fill { shape, .. } | Clip::Stroke { shape, .. } => shape,
        }),
        Command::Draw(id) => match scene.draw_op(*id) {
            Draw::Fill { shape, .. } | Draw::Stroke { shape, .. } => Some(shape),
            _ => None,
        },
        _ => None,
    }
}
pub(super) fn target_geometry(op: &mut FrameOp) -> Option<&mut Geometry> {
    match op {
        FrameOp::PushClip(c) => Some(match c {
            Clip::Fill { shape, .. } | Clip::Stroke { shape, .. } => shape,
        }),
        FrameOp::PushGroup(g) => g.clip.as_mut().map(|c| match c {
            Clip::Fill { shape, .. } | Clip::Stroke { shape, .. } => shape,
        }),
        FrameOp::Draw(FrameDraw::Fill { shape, .. } | FrameDraw::Stroke { shape, .. }) => {
            Some(shape)
        }
        _ => None,
    }
}
pub(super) fn source_stroke<'a>(scene: &'a Scene, c: &Command) -> Option<&'a Stroke> {
    match c {
        Command::PushClip(id) => match scene.clip(*id) {
            Clip::Stroke { stroke, .. } => Some(stroke),
            _ => None,
        },
        Command::PushGroup(id) => match &scene.group(*id).clip {
            Some(Clip::Stroke { stroke, .. }) => Some(stroke),
            _ => None,
        },
        Command::Draw(id) => match scene.draw_op(*id) {
            Draw::Stroke { stroke, .. } => Some(stroke),
            Draw::GlyphRun(g) => match &g.style {
                Style::Stroke(s) => Some(s),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}
pub(super) fn target_stroke(op: &mut FrameOp) -> Option<&mut Stroke> {
    match op {
        FrameOp::PushClip(Clip::Stroke { stroke, .. }) => Some(stroke),
        FrameOp::PushGroup(g) => match &mut g.clip {
            Some(Clip::Stroke { stroke, .. }) => Some(stroke),
            _ => None,
        },
        FrameOp::Draw(FrameDraw::Stroke { stroke, .. }) => Some(stroke),
        FrameOp::Draw(FrameDraw::GlyphRun(g)) => match &mut g.style {
            Style::Stroke(s) => Some(s),
            _ => None,
        },
        _ => None,
    }
}
pub(super) fn source_brush<'a>(scene: &'a Scene, c: &Command) -> Option<&'a Brush> {
    match c {
        Command::Draw(id) => match scene.draw_op(*id) {
            Draw::Fill { brush, .. } | Draw::Stroke { brush, .. } => Some(brush),
            Draw::GlyphRun(g) => Some(&g.brush),
            _ => None,
        },
        _ => None,
    }
}
pub(super) fn target_brush(op: &mut FrameOp) -> Option<&mut FrameBrush> {
    match op {
        FrameOp::Draw(FrameDraw::Fill { brush, .. } | FrameDraw::Stroke { brush, .. }) => {
            Some(brush)
        }
        FrameOp::Draw(FrameDraw::GlyphRun(g)) => Some(&mut g.brush),
        _ => None,
    }
}
pub(super) fn source_glyph<'a>(scene: &'a Scene, c: &Command) -> Option<&'a record::GlyphRun> {
    if let Command::Draw(id) = c
        && let Draw::GlyphRun(g) = scene.draw_op(*id)
    {
        Some(g)
    } else {
        None
    }
}
pub(super) fn context_text<'a>(scene: &'a Scene, c: &Command, field: u8) -> Option<&'a str> {
    let Command::PushContext(id) = c else {
        return None;
    };
    let ctx = scene.context(*id).as_ref(scene);
    match field {
        6 => match ctx.kind {
            imaging::ContextKindRef::Named(s) => Some(s),
            _ => None,
        },
        7 => match ctx.value {
            imaging::ContextValueRef::Str(s) => Some(s),
            _ => None,
        },
        8 => ctx.source.map(|s| s.file),
        _ => None,
    }
}
pub(super) fn field_size(scene: &Scene, c: &Command, field: u8, index: usize) -> Option<usize> {
    match field {
        0 => match source_geometry(scene, c)? {
            Geometry::Path(p) if index < p.elements().len() => Some(size_of::<PathEl>()),
            _ => None,
        },
        1 => (index < source_stroke(scene, c)?.dash_pattern.len()).then_some(size_of::<f64>()),
        2 => match source_brush(scene, c)? {
            Brush::Gradient(g) if index < g.stops.len() => Some(size_of::<peniko::ColorStop>()),
            _ => None,
        },
        3 => (index < source_glyph(scene, c)?.glyphs.len()).then_some(size_of::<record::Glyph>()),
        4 => (index < source_glyph(scene, c)?.normalized_coords.len())
            .then_some(size_of::<imaging::NormalizedCoord>()),
        5 => {
            if let Command::PushGroup(id) = c {
                (index < scene.group(*id).filters.len()).then_some(size_of::<Filter>())
            } else {
                None
            }
        }
        6..=8 => (index < context_text(scene, c, field)?.len()).then_some(1),
        _ => None,
    }
}
pub(super) fn copy_field(scene: &Scene, c: &Command, job: &mut CopyJob) -> Result<(), BuildError> {
    let i = job.index;
    let op = &mut job.command.op;
    let error = || BuildError {
        at: job.command.origin.at,
        kind: BuildErrorKind::InvalidRecord,
    };
    match job.field {
        0 => {
            let Geometry::Path(source) = source_geometry(scene, c).unwrap() else {
                unreachable!()
            };
            let Geometry::Path(target) = target_geometry(op).unwrap() else {
                unreachable!()
            };
            let el = source.elements()[i];
            if !valid_path_element(el) {
                return Err(error());
            }
            let points: &[_] = match &el {
                PathEl::MoveTo(p) | PathEl::LineTo(p) => std::slice::from_ref(p),
                _ => &[],
            };
            for p in points {
                job.ink = job.ink.union(InkBounds::Rect(Rect::from_points(*p, *p)));
            }
            match el {
                PathEl::QuadTo(a, b) => {
                    job.ink = job.ink.union(InkBounds::Rect(Rect::from_points(a, b)));
                }
                PathEl::CurveTo(a, b, c) => {
                    job.ink = job.ink.union(InkBounds::Rect(
                        Rect::from_points(a, b).union(Rect::from_points(c, c)),
                    ));
                }
                _ => {}
            }
            target.push(el);
        }
        1 => {
            let dash = source_stroke(scene, c).unwrap().dash_pattern[i];
            if !valid_dash(dash) {
                return Err(error());
            }
            target_stroke(op).unwrap().dash_pattern.push(dash);
        }
        2 => {
            let Brush::Gradient(source) = source_brush(scene, c).unwrap() else {
                unreachable!()
            };
            let Brush::Gradient(target) = target_brush(op).unwrap() else {
                unreachable!()
            };
            let stop = source.stops[i];
            if !valid_stop(stop) {
                return Err(error());
            }
            target.stops.push(stop);
        }
        3 => {
            let glyph = source_glyph(scene, c).unwrap().glyphs[i];
            if !valid_glyph(glyph) {
                return Err(error());
            }
            let FrameOp::Draw(FrameDraw::GlyphRun(g)) = op else {
                unreachable!()
            };
            g.glyphs.push(glyph);
        }
        4 => {
            let FrameOp::Draw(FrameDraw::GlyphRun(g)) = op else {
                unreachable!()
            };
            g.normalized_coords
                .push(source_glyph(scene, c).unwrap().normalized_coords[i]);
        }
        5 => {
            let Command::PushGroup(id) = c else {
                unreachable!()
            };
            let FrameOp::PushGroup(g) = op else {
                unreachable!()
            };
            let filter = scene.group(*id).filters[i];
            if !valid_filter(filter) {
                return Err(error());
            }
            g.filters.push(filter);
        }
        field => {
            let byte = context_text(scene, c, field).unwrap().as_bytes()[i];
            let FrameOp::PushContext(ctx) = op else {
                unreachable!()
            };
            let target = match field {
                6 => {
                    let FrameContextKind::Named(t) = &mut ctx.kind else {
                        unreachable!()
                    };
                    t
                }
                7 => {
                    let FrameContextValue::Str(t) = &mut ctx.value else {
                        unreachable!()
                    };
                    t
                }
                8 => &mut ctx.source.as_mut().unwrap().file,
                _ => unreachable!(),
            };
            target.0.push(byte);
        }
    }
    Ok(())
}
pub(super) fn initial_ink(op: &FrameOp) -> InkBounds {
    let geometry = match op {
        FrameOp::Draw(FrameDraw::Fill { shape, .. } | FrameDraw::Stroke { shape, .. }) => shape,
        _ => return InkBounds::Empty,
    };
    match geometry {
        Geometry::Rect(r) => InkBounds::Rect(*r),
        Geometry::RoundedRect(r) => InkBounds::Rect(r.rect()),
        Geometry::Path(_) => InkBounds::Empty,
    }
}
pub(super) fn draw_ink(op: &FrameOp, ink: InkBounds, aa: f64) -> InkBounds {
    match op {
        FrameOp::Draw(FrameDraw::Fill { transform, .. }) => {
            inflate_aa(ink.transformed(*transform), aa)
        }
        FrameOp::Draw(FrameDraw::Stroke {
            transform, stroke, ..
        }) => match ink {
            InkBounds::Rect(r) => inflate_aa(
                InkBounds::Rect(r.inflate(
                    stroke.width * stroke.miter_limit.max(1.),
                    stroke.width * stroke.miter_limit.max(1.),
                ))
                .transformed(*transform),
                aa,
            ),
            other => other,
        },
        // No finite Gaussian kernel cutoff has been fixed by this semantic profile.
        FrameOp::Draw(FrameDraw::BlurredRoundedRect(_)) => InkBounds::Unbounded,
        FrameOp::Draw(FrameDraw::GlyphRun(_)) => InkBounds::Unbounded,
        _ => ink,
    }
}
