//! Frozen physical views of ordinary effects on their original logical raster.
use super::{effect_chain, effect_plan, scopes::GroupPlan, source_crop::Crop};
use reel_ui::{
    frame::{FrameDrawing, FrameOp},
    stage::effect::EffectType,
};
#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub(super) struct Entry {
    pub push: usize,
    pub crop: Crop,
}

#[derive(Default)]
pub(super) struct Plan {
    pub entries: Vec<Entry>,
    pub collecting: bool,
}
impl Plan {
    pub fn begin(&mut self) {
        self.collecting = true;
        #[cfg(test)]
        {
            self.collecting = !testing::disabled();
        }
    }
    pub fn crop(&self, push: usize) -> Option<Crop> {
        self.entries
            .binary_search_by_key(&push, |entry| entry.push)
            .ok()
            .map(|index| self.entries[index].crop)
    }
}

/// Keep any whole-chain macrotile that the ordinary planner could select.
/// Large sources outside that path remain eligible for physical output windows.
pub(super) fn macro_source(
    drawing: &FrameDrawing,
    root: usize,
    groups: &GroupPlan,
    storage: [u32; 2],
) -> bool {
    let Some(chain) = effect_chain::plan(drawing.commands(), root, groups) else {
        return false;
    };
    if effect_chain::layout(storage, chain.count + 1).is_none() {
        return false;
    }
    std::iter::once(root)
        .chain(chain.effects[..chain.count].iter().copied())
        .all(|push| {
            let command = &drawing.commands()[push];
            let FrameOp::PushGroup(group) = &command.op else {
                return false;
            };
            group
                .effect
                .as_ref()
                .and_then(|effect| effect_plan::shader(drawing, effect, command.origin.at).ok())
                .is_some_and(|shader| {
                    shader.descriptor.output == EffectType::Vec4
                        && shader.descriptor.source_sampler
                        && shader.descriptor.textures == 0
                })
        })
}

#[cfg(test)]
pub(crate) mod testing {
    std::thread_local! { static DISABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
    pub(super) fn disabled() -> bool {
        DISABLED.get()
    }
    pub(crate) fn without<T>(run: impl FnOnce() -> T) -> T {
        struct Restore(bool);
        impl Drop for Restore {
            fn drop(&mut self) {
                DISABLED.set(self.0);
            }
        }
        let _restore = Restore(DISABLED.replace(true));
        run()
    }
}
