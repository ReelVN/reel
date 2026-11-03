//! Explain failed cache attempts using the same complete key as lookup.
use super::*;
use crate::profiling::CACHE_MISS_LABELS;

fn causes(key: Option<&Key>, probe: &Probe<'_>, retained: bool) -> [u64; CACHE_MISS_LABELS.len()] {
    let Some(key) = key else {
        let mut counts = [0; CACHE_MISS_LABELS.len()];
        counts[0] = 1;
        return counts;
    };
    let source_changed = key.source != probe.source;
    let (recording_identity, recording_pixels) = match (&key.source, &probe.source) {
        (Source::Recording(a), Source::Recording(b)) => (
            a.owner != b.owner || !Arc::ptr_eq(&a.scene, &b.scene) || a.local != b.local,
            !a.same_pixels(b),
        ),
        _ => (false, false),
    };
    let context = match (&key.grid.effect_context, &probe.grid.effect_context) {
        (Some(a), Some(b)) => a.changed_rows(b),
        _ => [false; 3],
    };
    [
        0,
        u64::from(source_changed && matches!(&probe.source, Source::Recording(_))),
        u64::from(source_changed && matches!(&probe.source, Source::Output(_))),
        u64::from(
            key.shader != probe.effect.shader
                || key.values != probe.effect.values
                || key.images != probe.effect.images,
        ),
        u64::from(
            key.bounds != rect(probe.effect.bounds)
                || key.coverage_invert != probe.effect.coverage_invert
                || key.resolution_scale != probe.effect.resolution_scale.to_bits(),
        ),
        u64::from(
            key.grid.source_size != probe.grid.source_size
                || key.grid.source_storage != probe.grid.source_storage,
        ),
        u64::from(key.grid.source_scissor != probe.grid.source_scissor),
        u64::from(
            key.grid.effect_transform != probe.grid.effect_transform
                || key.grid.effect_context != probe.grid.effect_context,
        ),
        u64::from(
            key.grid.output_size != probe.grid.output_size
                || key.grid.output_storage != probe.grid.output_storage,
        ),
        u64::from(key.grid.scissor != probe.grid.scissor),
        u64::from(key.grid.quality != probe.grid.quality),
        u64::from(key.matches(probe) && !retained),
        0,
        u64::from(recording_identity),
        u64::from(recording_pixels),
        u64::from(context[0]),
        u64::from(context[1]),
        u64::from(context[2]),
    ]
}

pub(super) fn record<T>(entries: &[Entry<T>], probe: &Probe<'_>) {
    let anchor = probe.source.anchor();
    let previous = entries
        .iter()
        .take_while(|entry| entry.key.source.anchor() == anchor)
        .find(|entry| entry.key.definition == probe.effect.definition);
    crate::profiling::record_effect(crate::profiling::EffectSample {
        definition: probe.effect.definition,
        variant: 3,
        cache_misses: causes(
            previous.map(|entry| &entry.key),
            probe,
            previous.is_some_and(|entry| entry.output.is_some()),
        ),
        ..Default::default()
    });
}

#[cfg(test)]
mod tests {
    use super::super::tests::{effect, grid, pool, probe};
    use super::*;

    #[test]
    fn miss_causes_preserve_overlapping_dependencies_and_distinguish_observations() {
        let pool = pool();
        let effect = effect();
        let scene = Arc::new(Scene::new());
        let key = Key::new(probe(&effect, grid(), scene.clone()), &pool).unwrap();
        let unchanged = probe(&effect, grid(), scene.clone());
        assert_eq!(
            causes(Some(&key), &unchanged, true),
            [0; CACHE_MISS_LABELS.len()]
        );
        assert_eq!(causes(Some(&key), &unchanged, false)[11], 1);
        assert_eq!(causes(None, &unchanged, false)[0], 1);
        let mut changed_grid = grid();
        changed_grid.source_scissor[2] -= 1;
        changed_grid.scissor[0] += 1;
        let changed = probe(&effect, changed_grid, scene);
        let counts = causes(Some(&key), &changed, true);
        assert_eq!(counts.iter().sum::<u64>(), 2);
        assert_eq!((counts[6], counts[9]), (1, 1));
        let changed = Probe::from_output(Token(9, 3), &effect, grid());
        let counts = causes(Some(&key), &changed, true);
        assert_eq!(counts[2], 1);
        assert_eq!(counts[11], 0);

        let changed = probe(&effect, grid(), Arc::new(Scene::new()));
        let counts = causes(Some(&key), &changed, true);
        assert_eq!((counts[1], counts[13], counts[14]), (1, 1, 0));
    }
}
