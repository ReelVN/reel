//! Reserve optional cache replacements before replay after stable observations.
use super::*;

/// Physical ordinary target sizes; duplicate sizes belong to distinct producers.
pub(crate) struct WarmTargets {
    pub sizes: Vec<[u32; 2]>,
    _metadata: BudgetTicket,
}

impl Candidate {
    /// Publish a missing replacement only after the same dependencies were
    /// observed in submitted work. Abandoned candidates cannot request storage.
    pub(crate) fn observe_for_target<T>(self, size: [u32; 2]) -> Pending<T> {
        Pending {
            warming: self.observed.then_some(size),
            key: self.key,
            token: self.token,
            output: None,
            replaces_source: false,
            eligible: self.retain,
            tail_only: false,
        }
    }
}

impl<T> Cache<T> {
    /// Copy recent requests before any backend allocation or reclamation, so
    /// resource preparation never executes while the cache mutex is held.
    pub(crate) fn warming_targets(&self, pool: &BudgetPool) -> Option<WarmTargets> {
        let state = self.0.lock().unwrap();
        let requested = |entry: &&Entry<T>| {
            entry.output.is_none()
                && entry.warming.is_some()
                && state.submitted.saturating_sub(entry.last_used) <= 1
        };
        let count = state.entries.iter().filter(requested).count();
        if count == 0 {
            return None;
        }
        let mut metadata = pool.reserve((count * size_of::<[u32; 2]>()) as u64).ok()?;
        let mut sizes = Vec::with_capacity(count);
        metadata
            .resize((sizes.capacity() * size_of::<[u32; 2]>()) as u64)
            .ok()?;
        sizes.extend(
            state
                .entries
                .iter()
                .filter(requested)
                .map(|entry| entry.warming.unwrap()),
        );
        Some(WarmTargets {
            sizes,
            _metadata: metadata,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{effect, grid, pool, probe};
    use super::*;

    #[test]
    fn cold_cache_requests_a_replacement_only_after_submitted_stability() {
        let pool = pool();
        let cache = Cache::<u32>::default();
        let mut effect = effect();
        let scene = Arc::new(Scene::new());
        let make = |effect: &FrameEffect| {
            cache
                .prepare(probe(effect, grid(), scene.clone()), &pool)
                .unwrap()
        };
        assert!(cache.publish(make(&effect).observe_for_target([32, 32]), &pool));
        assert!(cache.warming_targets(&pool).is_none());
        drop(make(&effect).observe_for_target::<u32>([32, 32]));
        assert!(
            cache.warming_targets(&pool).is_none(),
            "cancelled candidates are private"
        );
        assert!(cache.publish(make(&effect).observe_for_target([32, 32]), &pool));
        let bytes = pool.usage().bytes;
        let warm = cache.warming_targets(&pool).unwrap();
        assert_eq!(warm.sizes, [[32, 32]]);
        assert!(pool.usage().bytes > bytes);
        drop(warm);
        assert_eq!(pool.usage().bytes, bytes);
        effect.values[0] = EffectValue::Float(1f32.to_bits());
        assert!(cache.publish(make(&effect).observe_for_target([32, 32]), &pool));
        assert!(
            cache.warming_targets(&pool).is_none(),
            "changed inputs restart observation"
        );
        assert!(cache.publish(make(&effect).observe_for_target([32, 32]), &pool));
        assert!(cache.warming_targets(&pool).is_some());
        assert!(cache.publish(make(&effect).finish(7), &pool));
        assert!(
            cache.warming_targets(&pool).is_none(),
            "retained pixels already own storage"
        );
        assert_eq!(cache.lookup(&probe(&effect, grid(), scene)).unwrap().1, 7);
    }

    #[test]
    fn warming_preserves_multiplicity_and_expires_after_inactive_submissions() {
        let pool = pool();
        let cache = Cache::<u32>::default();
        let effect = effect();
        let scene = Arc::new(Scene::new());
        for _ in 0..2 {
            for owner in [1, 2] {
                let mut probe = probe(&effect, grid(), scene.clone());
                let Source::Recording(recording) = &mut probe.source else {
                    unreachable!()
                };
                recording.owner = owner;
                let candidate = cache.prepare(probe, &pool).unwrap();
                assert!(cache.publish(candidate.observe_for_target([32, 32]), &pool));
            }
        }
        assert_eq!(cache.warming_targets(&pool).unwrap().sizes, [[32, 32]; 2]);
        cache.submitted();
        assert!(cache.warming_targets(&pool).is_some());
        cache.submitted();
        assert!(cache.warming_targets(&pool).is_none());
    }

    #[test]
    fn retained_consumer_retires_only_its_prefix_warming_request() {
        let pool = pool();
        let cache = Cache::<u32>::default();
        let effect = effect();
        let scene = Arc::new(Scene::new());
        let mut unrelated = effect.clone();
        unrelated.definition = [52; 32];
        for _ in 0..2 {
            let first = cache
                .prepare(probe(&effect, grid(), scene.clone()), &pool)
                .unwrap();
            assert!(cache.publish(first.observe_for_target([32, 32]), &pool));
            let other = cache
                .prepare(probe(&unrelated, grid(), scene.clone()), &pool)
                .unwrap();
            assert!(cache.publish(other.observe_for_target([64, 32]), &pool));
        }
        let token = cache
            .lookup_content(&probe(&effect, grid(), scene.clone()))
            .unwrap();
        let downstream = || Probe::from_output(token, &effect, grid());
        let cancelled = cache
            .prepare(downstream(), &pool)
            .unwrap()
            .finish_replacing_source(23);
        drop(cancelled);
        assert!(
            cache
                .warming_targets(&pool)
                .unwrap()
                .sizes
                .contains(&[32, 32])
        );
        let observed = cache.prepare(downstream(), &pool).unwrap();
        assert!(cache.publish(observed.observe(), &pool));
        assert!(
            cache
                .warming_targets(&pool)
                .unwrap()
                .sizes
                .contains(&[32, 32])
        );
        let retained = cache.prepare(downstream(), &pool).unwrap();
        assert!(cache.publish(retained.finish_replacing_source(23), &pool));
        assert_eq!(
            cache.lookup_content(&probe(&effect, grid(), scene)),
            Some(token)
        );
        assert_eq!(cache.warming_targets(&pool).unwrap().sizes, [[64, 32]]);
        assert_eq!(cache.lookup(&downstream()).unwrap().1, 23);
    }

    #[test]
    fn sealed_pixels_fulfill_warming_even_when_not_selected_next_frame() {
        let pool = pool();
        let cache = Cache::<u32>::default();
        let effect = effect();
        let scene = Arc::new(Scene::new());
        let make = || {
            cache
                .prepare(probe(&effect, grid(), scene.clone()), &pool)
                .unwrap()
        };
        assert!(cache.publish(make().observe_for_target([32, 32]), &pool));
        assert!(cache.publish(make().observe_for_target([32, 32]), &pool));
        assert!(cache.warming_targets(&pool).is_some());
        let mut pending = make().observe_for_target([32, 32]);
        pending.materialize(7, false);
        let token = pending.token;
        assert!(cache.publish(pending, &pool));
        assert_eq!(cache.release_tail_outputs(|_| false), 1);
        assert!(cache.warming_targets(&pool).is_none());
        assert_eq!(
            cache.lookup_content(&probe(&effect, grid(), scene)),
            Some(token)
        );
    }
}
