//! Execute admitted parent operations in original order after their sources.
use super::*;
use crate::common::offscreen::ParentStep;
impl Stream<'_> {
    pub(super) fn source_commands(
        &self,
        group: &Scope<'_>,
        capture: bool,
    ) -> Option<(std::ops::Range<usize>, Affine, Scissor)> {
        group
            .source_input
            .filter(|_| {
                group.isolated
                    && !group.shared_source
                    && group.mask_effect.is_none()
                    && group.delayed.is_none()
                    && !capture
                    && self.drawing.transitions().is_empty()
                    && self.drawing.capture_exclusions().is_empty()
            })
            .zip(self.offset)
            .map(|((push, raster, region), offset)| (push..offset + self.next, raster, region))
    }
    pub(super) fn draw_selected(&self, plan: &crate::common::retained::Plan, index: usize) -> bool {
        self.offset
            .is_none_or(|offset| plan.selected(self.drawing, offset + index))
    }
    pub(super) fn draw_batch_end(
        &self,
        plan: &crate::common::retained::Plan,
        start: usize,
        end: usize,
    ) -> usize {
        plan.draw_batch_end(self.drawing, self.offset, start, end)
    }
}
impl Context<'_> {
    pub(super) fn combine_view(
        &mut self,
        target: &Target,
        source: &Target,
        view: crate::common::source_view::SourceView,
        alpha: f32,
        invert: f32,
    ) -> Result<(), GpuError> {
        self.composition(
            target,
            Composition {
                source,
                mask: source,
                backdrop: source,
                backdrop_origin: [0; 2],
                alpha,
                invert,
                operation: CompositeOperation::SourceOver,
                blend: 0,
                expression: None,
                source_origin: view.origin(),
            },
        )
    }
}
impl Context<'_> {
    fn combine_views(
        &mut self,
        target: &Target,
        pending: &[DeferredComposite],
    ) -> Result<(), GpuError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::CompositeBatch);
        let scissor = self.scissor;
        for item in pending {
            self.scissor = item.scissor;
            self.combine_view(target, &item.source, item.view, item.opacity, item.invert)?;
        }
        self.scissor = scissor;
        Ok(())
    }
}
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
                        if !current.draw_selected(&self.selection, next) {
                            next += 1;
                            continue;
                        }
                        self.scissor = current.scopes.last().scissor;
                        self.mesh_clip = current.scopes.last().mesh_clip;
                        let command = &current.commands[next];
                        let FrameOp::Draw(draw) = &command.op else {
                            unreachable!("planned parent draw")
                        };
                        self.at = Some(command.origin.at);
                        let end = current.draw_batch_end(&self.selection, next, range.end);
                        let consumed = self.image_batch(
                            target,
                            &current.commands[next..end],
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
