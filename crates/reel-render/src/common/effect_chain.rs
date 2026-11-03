//! Bounded, single-consumer effect chains on one unchanged source raster.
use super::{effect_plan, scopes::GroupPlan};
use reel_ui::frame::{FrameCommand, FrameDraw, FrameGroup, FrameOp};

pub(crate) const MAX_CELLS: usize = 4;

/// Cell zero rasterizes the source; later cells run these inner effects in order.
/// The outer effect retains the existing output and terminal-composite path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Chain {
    pub effects: [usize; MAX_CELLS - 1],
    pub count: usize,
}

fn ordinary(group: &FrameGroup) -> bool {
    group.effect.is_some()
        && group.mask_effect.is_none()
        && group.clip.is_none()
        && group.mask.is_none()
        && group.image_mask.is_none()
        && group.filters.is_empty()
        && group.invert == 0.
        && group.composite.blend == Default::default()
}

fn metadata(commands: &[FrameCommand], cursor: &mut usize) {
    while commands
        .get(*cursor)
        .is_some_and(|command| matches!(command.op, FrameOp::PushContext(_) | FrameOp::PopContext))
    {
        *cursor += 1;
    }
}

/// Prove a maximum four-cell chain without changing its authored operations.
/// The caller excludes capture/transition boundaries and verifies the prepared
/// color-shader formats, exact physical storage sizes and backend capacity.
pub(crate) fn plan(commands: &[FrameCommand], root: usize, groups: &GroupPlan) -> Option<Chain> {
    let FrameOp::PushGroup(group) = &commands.get(root)?.op else {
        return None;
    };
    if !ordinary(group) {
        return None;
    }
    let mut parent = group;
    let mut pushes = [0; MAX_CELLS];
    pushes[0] = root;
    let mut count = 1;
    let mut cursor = root + 1;
    loop {
        metadata(commands, &mut cursor);
        let Some(FrameCommand {
            op: FrameOp::PushGroup(child),
            ..
        }) = commands.get(cursor)
        else {
            break;
        };
        if child.effect.is_none() {
            break;
        }
        if count == MAX_CELLS
            || !groups.is_source_child(cursor)
            || !ordinary(child)
            || child.composite.alpha != 1.
            || !effect_plan::same_source_domain(child, Some(parent))
        {
            return None;
        }
        pushes[count] = cursor;
        count += 1;
        parent = child;
        cursor += 1;
    }
    if count < 2 {
        return None;
    }
    // Only ordinary fills and already-proved inline groups make the source.
    // A nested mask, effect, clip, backdrop blend or external producer rejects
    // the complete candidate before allocation or command submission.
    let mut depth = 0;
    let mut painted = false;
    loop {
        match &commands.get(cursor)?.op {
            FrameOp::PushContext(_) | FrameOp::PopContext => {}
            FrameOp::PushGroup(group)
                if groups.can_inline(cursor)
                    && group.effect.is_none()
                    && group.mask_effect.is_none() =>
            {
                depth += 1;
            }
            FrameOp::PopGroup if depth != 0 => depth -= 1,
            FrameOp::PopGroup => break,
            FrameOp::Draw(FrameDraw::Fill { composite, .. })
                if composite.blend == Default::default() =>
            {
                painted = true;
            }
            _ => return None,
        }
        cursor += 1;
    }
    if !painted {
        return None;
    }
    // Check every enclosing suffix as well as the cached sole-child proof.
    // All intermediate groups close without a draw, sibling or state boundary.
    for _ in 1..count {
        cursor += 1;
        metadata(commands, &mut cursor);
        if !matches!(commands.get(cursor)?.op, FrameOp::PopGroup) {
            return None;
        }
    }
    let mut effects = [0; MAX_CELLS - 1];
    for (cell, &push) in pushes[1..count].iter().rev().enumerate() {
        effects[cell] = push;
    }
    Some(Chain {
        effects,
        count: count - 1,
    })
}

/// Native macrotile cells need 128-pixel alignment and may not exceed 1024.
/// Macro targets have their own exact layout key; round only to hardware cells.
pub(crate) fn layout(storage: [u32; 2], cells: usize) -> Option<([u32; 2], [u32; 2])> {
    if !(2..=MAX_CELLS).contains(&cells) || storage.contains(&0) {
        return None;
    }
    let cell = storage.map(|n| n.checked_next_multiple_of(128));
    let cell = [cell[0]?, cell[1]?];
    if cell.iter().any(|&n| n > 1024) {
        return None;
    }
    Some((cell, if cells == 2 { [1, 2] } else { [2, 2] }))
}

#[cfg(test)]
mod tests;
