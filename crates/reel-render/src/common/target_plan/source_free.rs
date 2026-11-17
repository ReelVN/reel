//! Source-free output planning includes authored low-resolution reconstruction.
use super::*;
use crate::common::source_plan::{self, SourceFreeOutput, SourceFreeParent};
impl Input<'_> {
    pub(super) fn source_free(
        &self,
        current: &mut Stream<'_>,
        group: &FrameGroup,
        scratch: &mut Scratch,
        reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
        at: reel_ui::frame::RecordLocation,
    ) -> Result<bool, GpuError> {
        if let Some(offset) = current.offset
            && let Some(end) = self
                .selection
                .sources
                .end(current.drawing, offset + current.next - 1)
        {
            let parent = current.scopes.last();
            let (_, output) = source_plan::source_free(
                group,
                SourceFreeParent {
                    group: parent.and_then(|scope| scope.group),
                    source_transform: parent.and_then(|scope| scope.source_effect_transform),
                    accepts_forwarded_source: current.groups.is_source_child(current.next - 1)
                        && parent.is_some_and(|scope| scope.isolated),
                    shared_source: parent.is_some_and(|scope| scope.shared_source),
                },
                current.transform,
                current.size,
                self.max_dimension,
                self.samples,
                at,
            )?;
            let reduced = scratch.begin_resolution(current.size, group.effect.as_ref(), reserve)?;
            if output != SourceFreeOutput::Direct {
                scratch.acquire(current.size, reserve)?;
                scratch.release(current.size, reserve)?;
            }
            scratch.end_resolution(reduced, reserve)?;
            current.next = end - offset;
            return Ok(true);
        }
        Ok(false)
    }
}
