//! Replay a bounded source window's parent operations directly from the immutable
//! command stream. No per-parent-draw queue or copied draw payload is retained.
use super::{Entry, Plan};
use reel_ui::frame::{FrameCommand, FrameOp};
use std::ops::Range;

pub(crate) enum ParentStep {
    Sources(Range<usize>),
    Draws(Range<usize>),
}
pub(crate) struct ParentCursor {
    entries: Range<usize>,
    command: usize,
    source: usize,
}
impl Plan {
    /// The first producer identifies an independent window in this drawing.
    #[cfg(any(feature = "gxm", test))]
    pub(crate) fn window(&self, command: usize) -> Option<(usize, usize)> {
        let index = self
            .entries
            .binary_search_by_key(&command, |entry| entry.push)
            .ok()?;
        let first = self.entries[..=index]
            .iter()
            .rposition(|entry| entry.first)?;
        Some((self.entries[first].push, index - first))
    }
    /// Sources themselves retain their child draws. Only draws between sources
    /// in an admitted window are delayed to the ordered parent replay.
    pub(crate) fn defer_draw(&self, command: usize) -> bool {
        let index = self.entries.partition_point(|entry| entry.push <= command);
        index != 0 && {
            let entry = &self.entries[index - 1];
            !entry.last && command >= entry.end
        }
    }
    pub(crate) fn parent_cursor(&self, first: usize) -> ParentCursor {
        let start = self
            .entries
            .binary_search_by_key(&first, |entry| entry.push)
            .expect("scheduled first source");
        let end = start
            + self.entries[start..]
                .iter()
                .position(|entry| entry.last)
                .expect("bounded source window")
            + 1;
        ParentCursor {
            entries: start..end,
            command: first,
            source: 0,
        }
    }
    pub(crate) fn next_parent(
        &self,
        commands: &[FrameCommand],
        cursor: &mut ParentCursor,
    ) -> Option<ParentStep> {
        let entry = self
            .entries
            .get(cursor.entries.start)
            .filter(|_| cursor.entries.start < cursor.entries.end)?;
        skip_metadata(commands, &mut cursor.command, entry.push);
        if cursor.command < entry.push {
            let start = cursor.command;
            while cursor.command < entry.push
                && matches!(commands[cursor.command].op, FrameOp::Draw(_))
            {
                cursor.command += 1;
            }
            return Some(ParentStep::Draws(start..cursor.command));
        }
        let first = cursor.source;
        while cursor.entries.start < cursor.entries.end {
            let Entry { end, .. } = self.entries[cursor.entries.start];
            cursor.command = end;
            cursor.source += 1;
            cursor.entries.start += 1;
            if cursor.entries.start == cursor.entries.end {
                break;
            }
            let next = &self.entries[cursor.entries.start];
            skip_metadata(commands, &mut cursor.command, next.push);
            if cursor.command < next.push {
                break;
            }
        }
        Some(ParentStep::Sources(first..cursor.source))
    }
}

fn skip_metadata(commands: &[FrameCommand], next: &mut usize, end: usize) {
    while *next < end {
        match commands[*next].op {
            FrameOp::Draw(_) => break,
            // Cross only inline metadata or a proved invisible completed group.
            // A parent draw fixes the scope for the rest of this window, so all
            // delayed parent draws use the final source's exact parent state.
            FrameOp::PushGroup(_) => {
                let mut depth = 1;
                let mut close = *next + 1;
                while depth != 0 {
                    match commands[close].op {
                        FrameOp::PushGroup(_) => depth += 1,
                        FrameOp::PopGroup => depth -= 1,
                        _ => {}
                    }
                    close += 1;
                }
                // An inline group enclosing the next source can contain parent
                // draws before it. Enter that metadata rather than skip its body.
                *next = if close > end { *next + 1 } else { close };
            }
            _ => *next += 1,
        }
    }
}
