//! Execute admitted parent operations in original order after their sources.
use super::*;
use crate::common::offscreen::ParentStep;
impl Context<'_> {
    pub(super) fn scheduled_parent(
        &mut self,
        current: &Stream<'_>,
        pending: &[DeferredComposite],
    ) -> Result<(), GpuError> {
        // Empty sources must be initialized before any parent operation, so a
        // later composite cannot return to a source target in the parent phase.
        for item in pending {
            self.ensure_sampled(&item.source.view)?;
        }
        let target = &current.scopes.last().target;
        let mut cursor = self.offscreen.parent_cursor(pending[0].push);
        while let Some(step) = self.offscreen.next_parent(current.commands, &mut cursor) {
            match step {
                ParentStep::Sources(range) => self.combine_views(target, &pending[range])?,
                ParentStep::Draws(range) => {
                    let mut next = range.start;
                    while next < range.end {
                        self.scissor = current.scopes.last().scissor;
                        self.mesh_clip = current.scopes.last().mesh_clip;
                        let command = &current.commands[next];
                        let FrameOp::Draw(draw) = &command.op else {
                            unreachable!("planned parent draw")
                        };
                        self.at = Some(command.origin.at);
                        let consumed = self.image_batch(
                            target,
                            &current.commands[next..range.end],
                            current.transform,
                        )?;
                        if consumed == 0 {
                            self.draw(
                                current.drawing,
                                target,
                                draw,
                                current.transform,
                                command.origin,
                            )?;
                            next += 1;
                        } else {
                            next += consumed;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
