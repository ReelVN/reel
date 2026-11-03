//! Read an existing larger valid region without changing stored content identity.
use super::*;
use crate::common::scissor::Scissor;
use reel_ui::resources::EffectSourceFootprint;

fn region([x, y, width, height]: [u32; 4]) -> Scissor {
    Scissor {
        x,
        y,
        width,
        height,
    }
}
fn contains(outer: Scissor, inner: Scissor) -> bool {
    inner.empty() || outer.intersect(inner) == inner
}

impl<'a> Probe<'a> {
    /// Exclusive forwarding proves that only these demanded output pixels are
    /// read. The compiler footprint includes all source taps; the common mapper
    /// also includes bilinear neighbors on the exact uploaded float32 context.
    pub(super) fn with_source_demand(mut self, footprint: Option<EffectSourceFootprint>) -> Self {
        self.required = footprint.and_then(|footprint| {
            let mut required = self.grid.effect_context?.required(
                footprint,
                region(self.grid.scissor),
                self.grid.source_size,
            )?;
            if let Source::Recording(recording) = &self.source {
                required = required.intersect(recording.bounds);
            }
            contains(region(self.grid.source_scissor), required).then_some(required)
        });
        self
    }
}

impl Key {
    /// This is a read-only coverage relation. Cache admission and publication
    /// keep exact keys, so a newly drawn smaller target never inherits this token.
    pub(super) fn covers(&self, probe: &Probe<'_>) -> bool {
        let Some(required) = probe.required else {
            return false;
        };
        let mut grid = self.grid;
        grid.source_scissor = probe.grid.source_scissor;
        grid.scissor = probe.grid.scissor;
        if grid != probe.grid || !self.dependencies_match(probe) {
            return false;
        }
        if let (Source::Recording(old), Source::Recording(new)) = (&self.source, &probe.source)
            && old.bounds != new.bounds
        {
            return false;
        }
        let covered = contains(region(self.grid.scissor), region(probe.grid.scissor))
            && contains(region(self.grid.source_scissor), required);
        #[cfg(test)]
        if covered {
            testing::hit();
        }
        covered
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod testing {
    std::thread_local! {
        static HITS: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    }
    pub(super) fn hit() {
        HITS.with(|hits| hits.set(hits.get().map(|count| count + 1)));
    }
    pub(crate) fn collect<T>(work: impl FnOnce() -> T) -> (T, u64) {
        HITS.with(|hits| assert!(hits.replace(Some(0)).is_none()));
        let value = work();
        (value, HITS.with(|hits| hits.take().unwrap()))
    }
}
