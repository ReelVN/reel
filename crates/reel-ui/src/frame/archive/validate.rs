//! Validation at the saved drawing boundary, before publication to a renderer.
use super::*;

#[derive(Default)]
pub(super) struct Balance {
    scopes: Vec<ScopeKind>,
    contexts: usize,
}
impl Balance {
    pub fn command(&mut self, op: &FrameOp) -> Result<(), RetainError> {
        let invalid = || retained::error(BuildErrorKind::InvalidRecord);
        match op {
            FrameOp::PushClip(_) => self.scopes.push(ScopeKind::Clip),
            FrameOp::PushGroup(_) => self.scopes.push(ScopeKind::Group),
            FrameOp::PopClip if self.scopes.pop() != Some(ScopeKind::Clip) => return Err(invalid()),
            FrameOp::PopGroup if self.scopes.pop() != Some(ScopeKind::Group) => {
                return Err(invalid());
            }
            FrameOp::PushContext(_) => self.contexts += 1,
            FrameOp::PopContext => {
                self.contexts = self.contexts.checked_sub(1).ok_or_else(invalid)?
            }
            _ => {}
        }
        Ok(())
    }
    pub fn finish(&self) -> Result<(), RetainError> {
        if self.scopes.is_empty() && self.contexts == 0 {
            Ok(())
        } else {
            Err(retained::error(BuildErrorKind::InvalidRecord))
        }
    }
}

fn geometry(value: &Geometry) -> bool {
    match value {
        Geometry::Path(path) => path.elements().iter().copied().all(valid_path_element),
        _ => valid_geometry(value),
    }
}
fn stroke(value: &Stroke) -> bool {
    valid_stroke(value) && value.dash_pattern.iter().copied().all(valid_dash)
}
fn clip(value: &Clip) -> bool {
    valid_clip(value)
        && match value {
            Clip::Fill { shape, .. } => geometry(shape),
            Clip::Stroke {
                shape,
                stroke: value,
                ..
            } => geometry(shape) && stroke(value),
        }
}
fn brush(value: &FrameBrush) -> bool {
    valid_brush(value)
        && match value {
            Brush::Gradient(gradient) => gradient.stops.iter().copied().all(valid_stop),
            _ => true,
        }
}
fn text(value: &OwnedText) -> bool {
    std::str::from_utf8(&value.0).is_ok()
}
pub(super) fn mask(value: &FrameImageMask) -> bool {
    value.bounds.is_finite()
        && value.parameters.iter().all(|v| v.is_finite())
        && value.images.len() == value.program.sources().len()
}
pub(super) fn command(
    command: &FrameCommand,
    current_mask: Option<u32>,
    masks: &BTreeMap<u32, Arc<FrameMask>>,
) -> Result<(), RetainError> {
    validate_op(&command.op).map_err(retained::error)?;
    let valid = match &command.op {
        FrameOp::PushClip(value) => clip(value),
        FrameOp::PushGroup(value) => {
            value.clip.as_ref().is_none_or(clip)
                && value.filters.iter().copied().all(valid_filter)
                && value.invert.is_finite()
                && (0. ..=1.).contains(&value.invert)
                && valid_composite(value.composite)
                && finite(value.effect_to_target)
                && value.image_mask.as_ref().is_none_or(mask)
                && value.mask.is_none_or(|reference| {
                    finite(reference.transform)
                        && masks.contains_key(&reference.mask)
                        // FrameBuild allocates a mask before descending into its owned scene.
                        && current_mask.is_none_or(|id| reference.mask > id)
                })
                && match value.ink_bounds {
                    InkBounds::Rect(r) => r.is_finite(),
                    _ => true,
                }
        }
        FrameOp::Draw(draw) => {
            validate_draw(draw).map_err(retained::error)?;
            match draw {
                FrameDraw::Fill {
                    shape,
                    brush: value,
                    ..
                } => geometry(shape) && brush(value),
                FrameDraw::Stroke {
                    shape,
                    stroke: style,
                    brush: value,
                    ..
                } => geometry(shape) && stroke(style) && brush(value),
                FrameDraw::GlyphRun(value) => {
                    brush(&value.brush)
                        && value.glyphs.iter().copied().all(valid_glyph)
                        && match &value.style {
                            Style::Stroke(value) => stroke(value),
                            _ => true,
                        }
                }
                FrameDraw::BlurredRoundedRect(_) => true,
            }
        }
        FrameOp::PushContext(context) => {
            (match &context.kind {
                FrameContextKind::Named(value) => text(value),
                _ => true,
            }) && (match &context.value {
                FrameContextValue::Str(value) => text(value),
                _ => true,
            }) && context
                .source
                .as_ref()
                .is_none_or(|value| text(&value.file))
        }
        FrameOp::External(_) => return Err(retained::error(BuildErrorKind::UnsupportedRecord)),
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err(retained::error(BuildErrorKind::InvalidRecord))
    }
}

pub(super) fn transition(
    value: &SavedTransition,
    index: usize,
    prior: Option<&SavedTransition>,
    commands: &[Arc<FrameCommand>],
) -> Result<(), RetainError> {
    if value.previous >= index
        || value.commands.start > value.commands.end
        || value.commands.end > commands.len()
        || !value.progress.is_finite()
        || !(0. ..=1.).contains(&value.progress)
        || !finite(value.to_parent)
        || !finite(value.parent_to_window)
        || !finite(value.mask_to_window)
        || value.mask.as_ref().is_some_and(|m| !mask(m))
        || prior.is_some_and(|prior| prior.commands.end > value.commands.start)
    {
        return Err(retained::error(BuildErrorKind::InvalidRecord));
    }
    let mut depth = 0_usize;
    let mut contexts = 0_usize;
    for command in &commands[value.commands.clone()] {
        match command.op {
            FrameOp::PushClip(_) | FrameOp::PushGroup(_) => depth += 1,
            FrameOp::PopClip | FrameOp::PopGroup => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| retained::error(BuildErrorKind::InvalidRecord))?
            }
            FrameOp::PushContext(_) => contexts += 1,
            FrameOp::PopContext => {
                contexts = contexts
                    .checked_sub(1)
                    .ok_or_else(|| retained::error(BuildErrorKind::InvalidRecord))?
            }
            _ => {}
        }
    }
    if depth == 0 && contexts == 0 {
        Ok(())
    } else {
        Err(retained::error(BuildErrorKind::InvalidRecord))
    }
}
