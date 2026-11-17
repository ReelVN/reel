//! Replay the selected color effect while retaining its original output path.
use super::*;
use crate::common::{
    effect_plan,
    source_plan::{self, SourceFreeOutput, SourceFreeParent},
};
use reel_ui::frame::FrameGroup;

impl Context<'_> {
    pub(super) fn source_free_group(
        &mut self,
        current: &mut Stream<'_>,
        group: &FrameGroup,
        at: RecordLocation,
    ) -> Result<(), GpuError> {
        self.flush_draws()?;
        let parent = current.scopes.last();
        let (source_size, output_path) = source_plan::source_free(
            group,
            SourceFreeParent {
                group: parent.group,
                source_transform: parent.source_effect_transform,
                accepts_forwarded_source: current.groups.is_source_child(current.next - 1)
                    && parent.isolated,
                shared_source: parent.shared_source,
            },
            current.transform,
            self.size,
            self.gpu.adapter.limits().max_texture_dimension_2d,
            self.quality.samples,
            at,
        )?;
        let effect = group.effect.as_ref().expect("planned color effect");
        let transform = effect_plan::output_transform(
            group,
            parent.group.zip(parent.source_effect_transform),
            current.transform,
        );
        let target = parent.target().clone();
        let parent_scissor = self.scissor;
        self.scissor = Scissor::group_bounds(group, current.transform, self.size, false)
            .map_or(self.scissor, |bounds| self.scissor.intersect(bounds));
        let output = self.effect_draw(
            current.drawing,
            effect,
            transform,
            effect_plan::Source::unused(source_size),
            None,
            (output_path == SourceFreeOutput::Direct).then_some(effect_plan::Terminal {
                target: &target,
                opacity: group.composite.alpha,
                coverage: None,
            }),
            None,
            at,
        )?;
        if output_path == SourceFreeOutput::Forward {
            let previous = current
                .scopes
                .last_mut()
                .target
                .replace(output)
                .expect("source-free parent");
            self.recycle_target(previous)?;
        } else if output_path == SourceFreeOutput::Composite {
            let processed = output;
            self.combine(
                &target,
                &processed.view,
                &processed.view,
                group.composite.alpha,
                0.,
                CompositeOperation::SourceOver,
            )?;
            self.recycle_target(processed)?;
        }
        self.scissor = parent_scissor;
        Ok(())
    }
}
