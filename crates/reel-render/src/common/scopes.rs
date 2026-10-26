use super::GpuError;
use reel_ui::frame::{FrameCommand, FrameDraw, FrameGroup, FrameOp, RecordLocation};
use std::ops::Range;

/// Proves source-over isolation redundant once per command stream. The compact
/// result is independent of the backend and preserves command/capture identities.
pub(crate) struct GroupPlan {
    inline: Vec<u8>,
}
struct PlannedGroup {
    start: usize,
    source_over: bool,
    output_source_over: bool,
    retained_boundary: bool,
}
impl GroupPlan {
    pub(crate) fn new(
        commands: &[FrameCommand],
        offset: Option<usize>,
        transitions: impl Iterator<Item = Range<usize>>,
        capture_exclusions: impl Iterator<Item = Range<usize>>,
        mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Self, GpuError> {
        if !commands
            .iter()
            .any(|c| matches!(c.op, FrameOp::PushGroup(_)))
        {
            return Ok(Self { inline: Vec::new() });
        }
        let bytes = commands.len().div_ceil(8);
        reserve(bytes as u64)?;
        let mut inline = vec![0; bytes];
        let mut stack: Vec<PlannedGroup> = Vec::new();
        let mut transitions = transitions.peekable();
        let mut capture_exclusions = capture_exclusions.peekable();
        for (index, command) in commands.iter().enumerate() {
            let boundary = offset.is_some_and(|offset| {
                // Empty retained transitions occupy a gap before this command.
                let at = offset + index;
                touches_boundary(&mut transitions, at)
                    | touches_boundary(&mut capture_exclusions, at)
            });
            if boundary && let Some(group) = stack.last_mut() {
                group.source_over = false;
                group.retained_boundary = true;
            }
            match &command.op {
                FrameOp::PushGroup(group) => {
                    push_stack(
                        &mut stack,
                        PlannedGroup {
                            start: index,
                            output_source_over: group.composite.blend == Default::default(),
                            retained_boundary: boundary,
                            source_over: !boundary
                                && group.composite.alpha == 1.
                                && group.composite.blend == Default::default()
                                && group.clip.is_none()
                                && group.mask.is_none()
                                && group.image_mask.is_none()
                                && group.effect.is_none()
                                && group.mask_effect.is_none()
                                && group.filters.is_empty()
                                && group.invert == 0.,
                        },
                        &mut reserve,
                    )?;
                }
                FrameOp::PopGroup => {
                    let Some(group) = stack.pop() else {
                        // Replay reports malformed scope structure at its origin.
                        continue;
                    };
                    if group.source_over {
                        inline[group.start / 8] |= 1 << (group.start % 8);
                    }
                    // An isolated child's internal draws see its own transparent
                    // target. Only its final composite can depend on the parent
                    // backdrop. Preserve retained boundaries through every ancestor.
                    if let Some(parent) = stack.last_mut()
                        && (!group.output_source_over || group.retained_boundary)
                    {
                        parent.source_over = false;
                        parent.retained_boundary |= group.retained_boundary;
                    }
                }
                FrameOp::PushContext(_) | FrameOp::PopContext => {}
                other => {
                    let source_over = match other {
                        FrameOp::Draw(
                            FrameDraw::Fill { composite, .. } | FrameDraw::Stroke { composite, .. },
                        ) => composite.blend == Default::default(),
                        FrameOp::Draw(FrameDraw::GlyphRun(run)) => {
                            run.composite.blend == Default::default()
                        }
                        _ => false,
                    };
                    if !source_over && let Some(group) = stack.last_mut() {
                        group.source_over = false;
                    }
                }
            }
        }
        Ok(Self { inline })
    }

    pub(crate) fn can_inline(&self, push: usize) -> bool {
        self.inline
            .get(push / 8)
            .is_some_and(|bits| bits & (1 << (push % 8)) != 0)
    }
}

fn touches_boundary(
    ranges: &mut std::iter::Peekable<impl Iterator<Item = Range<usize>>>,
    at: usize,
) -> bool {
    while ranges
        .peek()
        .is_some_and(|range| range.end < at || range.end == at && !range.is_empty())
    {
        ranges.next();
    }
    ranges.peek().is_some_and(|range| range.start <= at)
}

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
        || group.effect.is_some()
        || group.mask_effect.is_some()
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

    fn ordinary_group() -> FrameGroup {
        FrameGroup {
            image_mask: None,
            effect: None,
            mask_effect: None,
            clip: None,
            mask: None,
            filters: vec![],
            composite: reel_ui::frame::GroupComposite {
                alpha: 1.,
                blend: Default::default(),
            },
            invert: 0.,
            owner: at(0).owner,
            effect_to_target: reel_ui::kurbo::Affine::IDENTITY,
            ink_bounds: reel_ui::frame::InkBounds::Unbounded,
        }
    }
    fn command(index: usize, op: FrameOp) -> FrameCommand {
        FrameCommand {
            origin: reel_ui::frame::CommandOrigin {
                source: 0,
                local: reel_ui::frame::LocalId::None,
                at: at(index),
            },
            op,
        }
    }
    fn translucent_fill(index: usize) -> FrameCommand {
        command(
            index,
            FrameOp::Draw(FrameDraw::Fill {
                transform: reel_ui::kurbo::Affine::IDENTITY,
                fill_rule: Default::default(),
                brush: reel_ui::peniko::Brush::Solid(reel_ui::peniko::Color::WHITE),
                brush_transform: None,
                shape: reel_ui::imaging::record::Geometry::Rect(reel_ui::kurbo::Rect::new(
                    0., 0., 40., 30.,
                )),
                composite: reel_ui::imaging::Composite {
                    alpha: 0.5,
                    ..Default::default()
                },
            }),
        )
    }

    #[test]
    fn isolated_child_internals_do_not_require_parent_isolation() {
        let mut commands = vec![
            command(0, FrameOp::PushGroup(ordinary_group())),
            translucent_fill(1),
            command(2, FrameOp::PushGroup(ordinary_group())),
            translucent_fill(3),
            translucent_fill(4),
            command(5, FrameOp::PopGroup),
            command(6, FrameOp::PopGroup),
        ];
        let plan = |commands: &[FrameCommand]| {
            GroupPlan::new(
                commands,
                Some(0),
                std::iter::empty(),
                std::iter::empty(),
                |_| Ok(()),
            )
            .unwrap()
        };
        let ordinary = plan(&commands);
        assert!(ordinary.can_inline(0) && ordinary.can_inline(2));
        if let FrameOp::Draw(FrameDraw::Fill { composite, .. }) = &mut commands[3].op {
            composite.blend = reel_ui::peniko::Mix::Multiply.into();
        }
        let dependent = plan(&commands);
        assert!(dependent.can_inline(0) && !dependent.can_inline(2));
        if let FrameOp::PushGroup(group) = &mut commands[2].op {
            group.composite.blend = reel_ui::peniko::Mix::Multiply.into();
        }
        let dependent_output = plan(&commands);
        assert!(!dependent_output.can_inline(0) && !dependent_output.can_inline(2));
        if let FrameOp::PushGroup(group) = &mut commands[2].op {
            group.composite.blend = Default::default();
        }
        commands[3] = translucent_fill(3);
        if let FrameOp::PushGroup(group) = &mut commands[2].op {
            group.composite.alpha = 0.5;
        }
        let faded = plan(&commands);
        assert!(faded.can_inline(0) && !faded.can_inline(2));
        if let FrameOp::PushGroup(group) = &mut commands[2].op {
            group.mask = Some(reel_ui::frame::MaskUse {
                mask: 0,
                transform: reel_ui::kurbo::Affine::IDENTITY,
            });
        }
        let masked = plan(&commands);
        assert!(masked.can_inline(0) && !masked.can_inline(2));
        for (transitions, captures) in [(vec![4..4], vec![]), (vec![], vec![3..5])] {
            let retained = GroupPlan::new(
                &commands,
                Some(0),
                transitions.into_iter(),
                captures.into_iter(),
                |_| Ok(()),
            )
            .unwrap();
            assert!(!retained.can_inline(0) && !retained.can_inline(2));
        }
    }

    #[test]
    fn group_plan_preserves_retained_gaps_capture_offsets_and_effect_boundaries() {
        let commands = vec![
            command(0, FrameOp::PushGroup(ordinary_group())),
            translucent_fill(1),
            translucent_fill(2),
            command(3, FrameOp::PopGroup),
        ];
        for (transitions, capture) in [(vec![22..22], vec![]), (vec![], vec![21..23])] {
            let blocked = GroupPlan::new(
                &commands,
                Some(20),
                transitions.into_iter(),
                capture.into_iter(),
                |_| Ok(()),
            )
            .unwrap();
            assert!(!blocked.can_inline(0));
        }
        let outside = GroupPlan::new(
            &commands,
            Some(20),
            [19..20, 24..24].into_iter(),
            std::iter::empty(),
            |_| Ok(()),
        )
        .unwrap();
        assert!(outside.can_inline(0));
        for update in [
            |g: &mut FrameGroup| g.invert = 1.,
            |g: &mut FrameGroup| {
                g.mask = Some(reel_ui::frame::MaskUse {
                    mask: 0,
                    transform: reel_ui::kurbo::Affine::IDENTITY,
                })
            },
            |g: &mut FrameGroup| g.composite.blend = reel_ui::peniko::Mix::Screen.into(),
        ] {
            let mut commands = commands.clone();
            if let FrameOp::PushGroup(group) = &mut commands[0].op {
                update(group);
            }
            let plan = GroupPlan::new(
                &commands,
                None,
                std::iter::empty(),
                std::iter::empty(),
                |_| Ok(()),
            )
            .unwrap();
            assert!(!plan.can_inline(0));
        }
    }

    #[test]
    fn group_plan_storage_is_compact_and_group_free_streams_allocate_nothing() {
        let fill = translucent_fill(0);
        GroupPlan::new(
            &[fill.clone()],
            Some(0),
            std::iter::empty(),
            std::iter::empty(),
            |_| panic!("group-free allocation"),
        )
        .unwrap();
        let depth = 1024;
        let mut commands = (0..depth)
            .map(|i| command(i, FrameOp::PushGroup(ordinary_group())))
            .collect::<Vec<_>>();
        commands.push(fill);
        commands.extend((0..depth).map(|i| command(depth + 1 + i, FrameOp::PopGroup)));
        let mut reserved = 0;
        let plan = GroupPlan::new(
            &commands,
            Some(0),
            std::iter::empty(),
            std::iter::empty(),
            |bytes| {
                reserved += bytes;
                Ok(())
            },
        )
        .unwrap();
        assert!((0..depth).all(|i| plan.can_inline(i)));
        assert_eq!(plan.inline.len(), commands.len().div_ceil(8));
        assert_eq!(
            reserved,
            (plan.inline.capacity() + depth * size_of::<PlannedGroup>()) as u64
        );
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
            effect: None,
            mask_effect: None,
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
