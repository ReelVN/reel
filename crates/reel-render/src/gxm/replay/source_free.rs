//! Replay the selected color effect while retaining its original output path.
use super::*;
use crate::common::{effect_plan, source_plan};
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
        let source = source_plan::layout(
            group,
            parent.group,
            current.transform,
            self.size,
            self.gpu.capabilities().max_texture_size,
            at,
        )?;
        let forward = current.groups.is_source_child(current.next - 1)
            && parent.isolated
            && effect_plan::forwards_source(group, &source, current.transform, self.size);
        let effect = group.effect.as_ref().expect("planned color effect");
        let transform = current.transform * group.effect_to_target;
        let target = parent.target.clone();
        let parent_scissor = self.scissor;
        self.scissor = Scissor::group_bounds(group, current.transform, self.size, false)
            .map_or(self.scissor, |bounds| self.scissor.intersect(bounds));
        let terminal = !forward && effect_plan::terminal_composite(group, 1, false);
        let output = self.effect_draw(
            current.drawing,
            effect,
            transform,
            effect_plan::Source::unused(source.size),
            None,
            terminal.then_some(effect_plan::Terminal {
                target: &target,
                opacity: group.composite.alpha,
                coverage: None,
            }),
            at,
        )?;
        if forward {
            let previous = std::mem::replace(&mut current.scopes.last_mut().target, output);
            self.recycle_target(previous)?;
        } else if !terminal {
            let processed = output;
            self.combine(
                &target,
                &processed,
                &processed,
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
