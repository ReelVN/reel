//! Restrict fresh raw source leases to their sole effect consumer's read region.
use super::*;

impl Context<'_> {
    pub(super) fn restrict_source_initialization(
        &mut self,
        target: &mut Target,
        initialize: Scissor,
        current: &Stream<'_>,
    ) -> Result<(), GpuError> {
        if self.capture
            || self.warm.is_some()
            || current.retained_context != 0
            || current.offset.is_none()
            || !current.drawing.transitions().is_empty()
            || !current.drawing.capture_exclusions().is_empty()
            || target.macrotile_extent().is_some()
            || target.is_effect_pinned()
            || !self.target_writes.pending(target.slot)
            || initialize == Scissor::full(target.view.size())
        {
            return Ok(());
        }
        let Some(end) =
            crate::common::effect_result::source_end(current.commands, current.next - 1)
        else {
            return Ok(());
        };
        // Raw geometry and neutral wrappers can share this fresh source. Nested
        // effects, clips and external producers retain their own complete domains.
        if !current.commands[current.next..end - 1]
            .iter()
            .enumerate()
            .all(|(index, command)| match &command.op {
                FrameOp::Draw(_)
                | FrameOp::PushContext(_)
                | FrameOp::PopContext
                | FrameOp::PopGroup => true,
                FrameOp::PushGroup(_) => current.groups.can_inline(current.next + index),
                _ => false,
            })
        {
            return Ok(());
        }
        target.framebuffer = target.framebuffer.with_write_region(Some([
            initialize.x,
            initialize.y,
            initialize.width,
            initialize.height,
        ]))?;
        #[cfg(feature = "profiling")]
        {
            target.trace_write_region(Some([
                initialize.x,
                initialize.y,
                initialize.width,
                initialize.height,
            ]));
            target.trace_event(
                crate::profiling::TraceKind::TargetInit,
                "restricted-write-region",
            );
        }
        // A restricted raw view cannot become an immutable effect-cache result.
        target.effect_cache_allowed = false;
        // Empty sources reach first initialization through ensure_sampled rather
        // than a writer. Both paths must resolve this same per-use framebuffer.
        self.targets[target.slot] = target.clone();
        Ok(())
    }
}
