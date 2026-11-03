//! Execute admitted parent operations in original order after their sources.
use super::*;
use crate::common::offscreen::ParentStep;
impl Stream<'_> {
    pub(super) fn source_commands(
        &self,
        group: &Scope<'_>,
        capture: bool,
    ) -> Option<crate::common::effect_result::SourceCommands> {
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
            .map(
                |((push, raster, region), offset)| crate::common::effect_result::SourceCommands {
                    commands: push..offset + self.next,
                    transform: raster,
                    region,
                    early_reusable: group.early_reusable,
                },
            )
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
    pub(super) fn defer_composite<'a>(
        &mut self,
        current: &mut Stream<'a>,
        item: DeferredComposite<'a>,
        last: bool,
        shared: bool,
    ) -> Result<(), GpuError> {
        push_stack(&mut current.deferred, item, |bytes| self.transient(bytes))?;
        if last {
            let mut pending = std::mem::take(&mut current.deferred);
            self.scheduled_parent(current, &pending)?;
            for (index, item) in pending.drain(..).enumerate() {
                if (!shared || index == 0)
                    && self
                        .offscreen
                        .entry(item.push)
                        .unwrap()
                        .canonical_push
                        .is_none()
                {
                    self.recycle_target(item.source)?;
                }
            }
            current.deferred = pending;
        }
        Ok(())
    }

    fn combine_views(
        &mut self,
        drawing: &FrameDrawing,
        target: &Target,
        pending: &[DeferredComposite<'_>],
    ) -> Result<(), GpuError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::CompositeBatch);
        let scissor = self.scissor;
        let size = self.size;
        let mesh_clip = self.mesh_clip;
        for item in pending {
            self.scissor = item.scissor;
            self.size = item.size;
            self.mesh_clip = item.mesh_clip;
            if let Some((effect, transform, at)) = item.effect {
                self.effect_into(
                    drawing,
                    effect,
                    transform,
                    &item.source,
                    target,
                    item.opacity,
                    None,
                    at,
                )?;
                continue;
            }
            let composite = reel_ui::frame::GroupComposite {
                alpha: item.opacity,
                blend: item.blend,
            };
            if let Some(view) = item.view {
                if item.blend == Default::default() {
                    self.combine_view(target, &item.source, view, item.opacity, item.invert)?;
                } else {
                    self.combine_cropped(target, &item.source, view, composite, item.invert)?;
                }
            } else {
                self.combine_group(
                    target,
                    &item.source,
                    &item.source,
                    GroupPaint {
                        source_origin: [0; 2],
                        composite,
                        invert: item.invert,
                        operation: CompositeOperation::SourceOver,
                        image_mask: None,
                    },
                )?;
            }
        }
        self.scissor = scissor;
        self.size = size;
        self.mesh_clip = mesh_clip;
        Ok(())
    }
}
impl Context<'_> {
    pub(super) fn scheduled_parent(
        &mut self,
        current: &Stream<'_>,
        pending: &[DeferredComposite<'_>],
    ) -> Result<(), GpuError> {
        // Empty sources must be initialized before any parent operation, so a
        // later composite cannot return to a source target in the parent phase.
        for item in pending {
            self.ensure_sampled(&item.source.view)?;
        }
        let target = current.scopes.last().target();
        let mut cursor = self.offscreen.parent_cursor(pending[0].push);
        while let Some(step) = self.offscreen.next_parent(current.commands, &mut cursor) {
            match step {
                ParentStep::Sources(range) => {
                    self.combine_views(current.drawing, target, &pending[range])?;
                }
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
