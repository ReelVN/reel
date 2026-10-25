use super::GpuError;
use reel_ui::frame::{FrameCommand, FrameDraw, FrameGroup, FrameOp, RecordLocation};

/// A flat clip can move to ordinary draw geometry when it crosses no retained
/// transition/capture boundary. Child scopes keep their existing isolation.
pub(crate) fn flat_clip(
    drawing: &reel_ui::frame::FrameDrawing,
    commands: &[FrameCommand],
    next: usize,
    offset: Option<usize>,
    transitions: bool,
    capture: bool,
) -> bool {
    let mut end = next;
    loop {
        match commands.get(end).map(|command| &command.op) {
            Some(FrameOp::PopClip) => break,
            Some(FrameOp::Draw(_) | FrameOp::PushContext(_) | FrameOp::PopContext) => end += 1,
            _ => return false,
        }
    }
    let Some(offset) = offset else {
        return true;
    };
    let start = offset + next;
    let end = offset + end + 1;
    !(transitions
        && drawing
            .transitions()
            .iter()
            .any(|t| t.commands.start < end && t.commands.end > start)
        || capture
            && drawing
                .capture_exclusions()
                .iter()
                .any(|range| range.start < end && range.end > start))
}

/// A single source-over rectangle can carry its group's opacity directly.
/// Multiple draws and glyph runs retain isolation because their pixels can overlap.
/// The caller keeps transition and capture boundaries outside this fused pair.
pub(crate) fn inline_group_fill(
    group: &FrameGroup,
    commands: &[FrameCommand],
) -> Option<(FrameDraw, RecordLocation)> {
    use reel_ui::{imaging::record::Geometry, peniko::Brush};
    if group.clip.is_some()
        || group.mask.is_some()
        || group.image_mask.is_some()
        || !group.filters.is_empty()
        || group.invert != 0.
        || group.composite.blend != Default::default()
    {
        return None;
    }
    let [
        command,
        FrameCommand {
            op: FrameOp::PopGroup,
            ..
        },
        ..,
    ] = commands
    else {
        return None;
    };
    let FrameOp::Draw(FrameDraw::Fill {
        shape: Geometry::Rect(rect),
        transform,
        fill_rule,
        brush,
        brush_transform,
        composite,
    }) = &command.op
    else {
        return None;
    };
    if composite.blend != Default::default() || !matches!(brush, Brush::Solid(_) | Brush::Image(_))
    {
        return None;
    }
    Some((
        FrameDraw::Fill {
            shape: Geometry::Rect(*rect),
            transform: *transform,
            fill_rule: *fill_rule,
            brush: brush.clone(),
            brush_transform: *brush_transform,
            composite: reel_ui::imaging::Composite {
                alpha: composite.alpha * group.composite.alpha,
                blend: composite.blend,
            },
        },
        command.origin.at,
    ))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Clip,
    Group,
}
pub(crate) struct Entry<T> {
    pub(crate) at: RecordLocation,
    pub(crate) value: T,
    kind: Kind,
}
/// The replay stack owns its payloads. Validation precedes allocation/removal.
pub(crate) struct Scopes<T> {
    root: T,
    nested: Vec<Entry<T>>,
}

pub(crate) fn push_stack<T>(
    stack: &mut Vec<T>,
    entry: T,
    reserve: impl FnOnce(u64) -> Result<(), GpuError>,
) -> Result<(), GpuError> {
    if stack.len() == stack.capacity() {
        let additional = stack.capacity().max(1);
        let bytes = additional
            .checked_mul(size_of::<T>())
            .ok_or(GpuError::InvalidSize)?;
        reserve(bytes as u64)?;
        stack.reserve_exact(additional);
    }
    stack.push(entry);
    Ok(())
}

impl<T> Scopes<T> {
    pub(crate) fn new(root: T) -> Self {
        Self {
            root,
            nested: Vec::new(),
        }
    }
    pub(crate) fn push(
        &mut self,
        kind: Kind,
        at: RecordLocation,
        value: T,
        reserve: impl FnOnce(u64) -> Result<(), GpuError>,
    ) -> Result<(), GpuError> {
        push_stack(&mut self.nested, Entry { at, value, kind }, reserve)
    }
    pub(crate) fn pop(&mut self, kind: Kind, at: RecordLocation) -> Result<Entry<T>, GpuError> {
        match self.nested.last() {
            None => {
                return Err(GpuError::InvalidRecord {
                    at,
                    record: "scope underflow",
                });
            }
            Some(entry) if entry.kind != kind => {
                return Err(GpuError::InvalidRecord {
                    at,
                    record: "mismatched scope terminator",
                });
            }
            _ => (),
        }
        Ok(self.nested.pop().expect("validated scope"))
    }
    pub(crate) fn last(&self) -> &T {
        self.nested.last().map_or(&self.root, |entry| &entry.value)
    }
    pub(crate) fn finish(self) -> Result<T, GpuError> {
        if let Some(entry) = self.nested.last() {
            return Err(GpuError::InvalidRecord {
                at: entry.at,
                record: "unclosed scope",
            });
        }
        Ok(self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_ui::core::Widget;
    fn at(command: usize) -> RecordLocation {
        // One stable owner is needed to compare locations across calls.
        static OWNER: std::sync::OnceLock<reel_ui::core::WidgetId> = std::sync::OnceLock::new();
        RecordLocation {
            fragment: 0,
            command,
            mask_depth: 0,
            owner: *OWNER.get_or_init(|| reel_ui::widgets::Label::new("").prepare().id()),
        }
    }
    #[test]
    fn single_rectangle_opacity_is_folded_without_changing_the_recording() {
        use reel_ui::{
            frame::{CommandOrigin, GroupComposite, InkBounds, LocalId},
            imaging::{Composite, record::Geometry},
            kurbo::{Affine, Rect},
            peniko::{Brush, Color, Fill},
        };
        let mut group = FrameGroup {
            image_mask: None,
            clip: None,
            mask: None,
            filters: vec![],
            composite: GroupComposite {
                alpha: 0.4,
                blend: Default::default(),
            },
            invert: 0.,
            owner: at(0).owner,
            effect_to_target: Affine::IDENTITY,
            ink_bounds: InkBounds::Unbounded,
        };
        let command = |index, op| FrameCommand {
            origin: CommandOrigin {
                source: 0,
                local: LocalId::None,
                at: at(index),
            },
            op,
        };
        let draw = command(
            1,
            FrameOp::Draw(FrameDraw::Fill {
                transform: Affine::IDENTITY,
                fill_rule: Fill::NonZero,
                brush: Brush::Solid(Color::WHITE),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(0., 0., 40., 30.)),
                composite: Composite {
                    alpha: 0.5,
                    ..Default::default()
                },
            }),
        );
        let body = [draw.clone(), command(2, FrameOp::PopGroup)];
        let (FrameDraw::Fill { composite, .. }, location) =
            inline_group_fill(&group, &body).unwrap()
        else {
            panic!("fused rectangle");
        };
        assert_eq!(composite.alpha, 0.2);
        assert_eq!(location, at(1));
        assert!(
            matches!(&body[0].op, FrameOp::Draw(FrameDraw::Fill { composite, .. }) if composite.alpha == 0.5)
        );
        // Overlapping drawing remains isolated; multiplying each draw's alpha
        // would change its composition inside the group.
        assert!(inline_group_fill(&group, &[draw.clone(), draw, body[1].clone()]).is_none());
        group.invert = 1.;
        assert!(inline_group_fill(&group, &body).is_none());
        group.invert = 0.;
        group.composite.blend = reel_ui::peniko::Mix::Screen.into();
        assert!(inline_group_fill(&group, &body).is_none());
    }
    #[test]
    fn stack_growth_reserves_capacity_before_mutation_without_a_depth_ceiling() {
        let mut stack = Scopes::new(());
        let mut charged = 0;
        for index in 0..128 {
            stack
                .push(Kind::Group, at(index), (), |bytes| {
                    charged += bytes;
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(
            charged,
            (stack.nested.capacity() * size_of::<Entry<()>>()) as u64
        );
        assert!(matches!(
            stack.push(Kind::Clip, at(128), (), |bytes| {
                Err(GpuError::Budget {
                    domain: "transient",
                    requested: bytes,
                    available: 0,
                })
            }),
            Err(GpuError::Budget { .. })
        ));
        assert_eq!(stack.nested.len(), 128);
        for index in (0..128).rev() {
            assert_eq!(stack.pop(Kind::Group, at(index)).unwrap().at, at(index));
        }
        stack.finish().unwrap();
    }
    #[test]
    fn malformed_structure_is_localized_and_permanent() {
        let mut stack = Scopes::new(());
        assert!(
            matches!(stack.pop(Kind::Clip, at(1)), Err(GpuError::InvalidRecord { at: location, record: "scope underflow" }) if location == at(1))
        );
        stack.push(Kind::Group, at(2), (), |_| Ok(())).unwrap();
        assert!(
            matches!(stack.pop(Kind::Clip, at(3)), Err(GpuError::InvalidRecord { at: location, record: "mismatched scope terminator" }) if location == at(3))
        );
        let error = stack.finish().unwrap_err();
        assert!(
            matches!(error, GpuError::InvalidRecord { at: location, record: "unclosed scope" } if location == at(2))
        );
        assert!(!error.is_retryable());
    }
}
