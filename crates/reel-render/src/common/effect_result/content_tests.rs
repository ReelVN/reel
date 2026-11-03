//! Dependency identity survives independently of cached intermediate textures.
use super::tests::{effect, grid, pool, probe};
use super::*;

#[test]
fn observed_chain_keeps_content_versions_without_retaining_intermediate_pixels() {
    let pool = pool();
    let cache = Cache::<u32>::default();
    let mut first = effect();
    let mut second = effect();
    second.definition = [8; 32];
    let scene = Arc::new(Scene::new());
    let upstream = cache
        .prepare(probe(&first, grid(), scene.clone()), &pool)
        .unwrap();
    let original = upstream.token;
    assert!(cache.publish(upstream.observe(), &pool));
    let downstream = cache
        .prepare(Probe::from_output(original, &second, grid()), &pool)
        .unwrap();
    let final_token = downstream.token;
    assert!(cache.publish(downstream.finish(99), &pool));
    assert!(
        cache
            .lookup(&probe(&first, grid(), scene.clone()))
            .is_none()
    );
    let repeated = cache
        .prepare(probe(&first, grid(), scene.clone()), &pool)
        .unwrap();
    assert_eq!(repeated.token, original);
    assert_eq!(
        cache.lookup(&Probe::from_output(repeated.token, &second, grid())),
        Some((final_token, 99))
    );
    drop(repeated);
    first.values[0] = EffectValue::Float(2f32.to_bits());
    let changed = cache
        .prepare(probe(&first, grid(), scene.clone()), &pool)
        .unwrap();
    assert_ne!(changed.token, original);
    assert_eq!(changed.token.1, original.1);
    assert!(
        cache
            .lookup(&Probe::from_output(changed.token, &second, grid()))
            .is_none()
    );
    // Cancelling changed work leaves the submitted identity and its consumers.
    drop(changed);
    first.values[0] = EffectValue::Float(0f32.to_bits());
    assert_eq!(
        cache
            .prepare(probe(&first, grid(), scene.clone()), &pool)
            .unwrap()
            .token,
        original
    );
    cache.clear();
    let fresh = cache.prepare(probe(&first, grid(), scene), &pool).unwrap();
    assert_ne!(fresh.token, original);
}

#[test]
fn physical_pressure_releases_pixels_without_changing_dependency_versions() {
    let pool = pool();
    let cache = Cache::<Arc<u32>>::default();
    let effect = effect();
    let scene = Arc::new(Scene::new());
    let pixels = Arc::new(7);
    let retired = Arc::downgrade(&pixels);
    let candidate = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    let token = candidate.token;
    assert!(cache.publish(candidate.finish(pixels), &pool));
    let reader = cache
        .lookup(&probe(&effect, grid(), scene.clone()))
        .unwrap()
        .1;
    cache.release_outputs();
    assert!(
        cache
            .lookup(&probe(&effect, grid(), scene.clone()))
            .is_none()
    );
    assert_eq!(
        cache.lookup_content(&probe(&effect, grid(), scene.clone())),
        Some(token)
    );
    assert!(retired.upgrade().is_some());
    drop(reader);
    assert!(retired.upgrade().is_none());
    let repeated = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    assert_eq!(repeated.token, token);
    assert!(cache.publish(repeated.finish(Arc::new(7)), &pool));
    assert_eq!(
        cache.lookup(&probe(&effect, grid(), scene)).unwrap().0,
        token
    );
}

#[test]
fn retained_consumers_replace_prefix_pixels_without_retiring_content_or_readers() {
    let pool = pool();
    let cache = Cache::<Arc<u32>>::default();
    let effect = effect();
    let scene = Arc::new(Scene::new());
    let first = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    let first_token = first.token;
    let first_pixels = Arc::new(17);
    let first_owner = Arc::downgrade(&first_pixels);
    assert!(cache.publish(first.finish(first_pixels), &pool));
    let reader = cache
        .lookup(&probe(&effect, grid(), scene.clone()))
        .unwrap()
        .1;
    let middle_probe = || Probe::from_output(first_token, &effect, grid());
    let middle = cache.prepare(middle_probe(), &pool).unwrap();
    let middle_token = middle.token;
    let middle_pixels = Arc::new(23);
    let middle_owner = Arc::downgrade(&middle_pixels);
    assert!(cache.publish(middle.finish_replacing_source(middle_pixels), &pool));
    assert_eq!(
        cache.lookup_content(&probe(&effect, grid(), scene.clone())),
        Some(first_token)
    );
    assert!(
        cache
            .lookup(&probe(&effect, grid(), scene.clone()))
            .is_none()
    );
    assert_eq!(*reader, 17);
    assert!(first_owner.upgrade().is_some());
    drop(reader);
    assert!(first_owner.upgrade().is_none());
    let final_probe = || Probe::from_output(middle_token, &effect, grid());
    let last = cache.prepare(final_probe(), &pool).unwrap();
    let final_token = last.token;
    assert!(cache.publish(last.finish_replacing_source(Arc::new(31)), &pool));
    assert!(middle_owner.upgrade().is_none());
    assert_eq!(cache.lookup_content(&middle_probe()), Some(middle_token));
    assert!(cache.lookup(&middle_probe()).is_none());
    let (token, pixels) = cache.lookup(&final_probe()).unwrap();
    assert_eq!(token, final_token);
    assert_eq!(*pixels, 31);
    assert_eq!(
        cache
            .prepare(probe(&effect, grid(), scene), &pool)
            .unwrap()
            .token,
        first_token
    );
}

#[test]
fn observed_and_cancelled_consumers_keep_their_prefix_pixels() {
    let pool = pool();
    let cache = Cache::<u32>::default();
    let effect = effect();
    let scene = Arc::new(Scene::new());
    let first = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    let token = first.token;
    assert!(cache.publish(first.finish(17), &pool));
    let downstream = || Probe::from_output(token, &effect, grid());
    let abandoned = cache
        .prepare(downstream(), &pool)
        .unwrap()
        .finish_replacing_source(23);
    drop(abandoned);
    assert_eq!(
        cache.lookup(&probe(&effect, grid(), scene.clone())),
        Some((token, 17))
    );
    let observation = cache.prepare(downstream(), &pool).unwrap();
    assert!(cache.publish(observation.observe(), &pool));
    assert_eq!(
        cache.lookup(&probe(&effect, grid(), scene)),
        Some((token, 17))
    );
    assert!(cache.lookup_content(&downstream()).is_some());
    assert!(cache.lookup(&downstream()).is_none());
}

#[test]
fn failed_consumer_publication_keeps_the_submitted_prefix() {
    let pool = pool();
    let cache = Cache::<u32>::default();
    let effect = effect();
    let scene = Arc::new(Scene::new());
    let first = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    let token = first.token;
    assert!(cache.publish(first.finish(17), &pool));
    let mut unrelated = effect.clone();
    unrelated.definition = [39; 32];
    let other = cache
        .prepare(probe(&unrelated, grid(), scene.clone()), &pool)
        .unwrap();
    assert!(cache.publish(other.finish(99), &pool));
    // Both existing metadata slots are occupied. Preparing the consumer is
    // private; publishing it must admit a larger metadata allocation.
    let downstream = || Probe::from_output(token, &effect, grid());
    let candidate = cache.prepare(downstream(), &pool).unwrap();
    let remaining = 65536 - pool.usage().bytes;
    let pressure = pool.reserve(remaining).unwrap();
    assert!(!cache.publish(candidate.finish_replacing_source(23), &pool));
    drop(pressure);
    assert_eq!(
        cache.lookup(&probe(&effect, grid(), scene)),
        Some((token, 17))
    );
    assert!(cache.lookup_content(&downstream()).is_none());
}

#[test]
fn prefix_retirement_matches_exact_content_version_within_the_same_family() {
    let pool = pool();
    let cache = Cache::<u32>::default();
    let mut effect = effect();
    let scene = Arc::new(Scene::new());
    let first = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    let old = first.token;
    assert!(cache.publish(first.finish(17), &pool));
    let downstream_effect = effect.clone();
    let downstream = cache
        .prepare(Probe::from_output(old, &downstream_effect, grid()), &pool)
        .unwrap();
    effect.values[0] = EffectValue::Float(1f32.to_bits());
    let changed = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    let new = changed.token;
    assert_ne!(new, old);
    assert_eq!(new.1, old.1);
    assert!(cache.publish(changed.observe(), &pool));
    let stable = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    assert!(cache.publish(stable.finish(19), &pool));
    assert!(cache.publish(downstream.finish_replacing_source(23), &pool));
    assert_eq!(
        cache.lookup(&probe(&effect, grid(), scene)),
        Some((new, 19))
    );
}

#[test]
fn retained_consumers_without_an_early_reuse_proof_keep_source_pixels() {
    let pool = pool();
    let cache = Cache::<u32>::default();
    let effect = effect();
    let scene = Arc::new(Scene::new());
    let first = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    let token = first.token;
    assert!(cache.publish(first.finish(17), &pool));
    let downstream = || Probe::from_output(token, &effect, grid());
    let consumer = cache.prepare(downstream(), &pool).unwrap();
    assert!(cache.publish(consumer.finish(23), &pool));
    assert_eq!(
        cache.lookup(&probe(&effect, grid(), scene)),
        Some((token, 17))
    );
    assert_eq!(cache.lookup(&downstream()).unwrap().1, 23);
}

#[test]
fn sealing_an_observation_preserves_its_identity_and_commit_boundary() {
    let pool = pool();
    let cache = Cache::<Arc<u32>>::default();
    let effect = effect();
    let scene = Arc::new(Scene::new());
    let candidate = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    let token = candidate.token;
    let pixels = Arc::new(37);
    let owner = Arc::downgrade(&pixels);
    let mut pending = candidate.observe();
    pending.materialize(pixels, false);
    assert_eq!(pending.token, token);
    assert!(
        cache
            .lookup_content(&probe(&effect, grid(), scene.clone()))
            .is_none()
    );
    drop(pending);
    assert!(owner.upgrade().is_none());
    let candidate = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    let token = candidate.token;
    let mut pending = candidate.observe();
    pending.materialize(Arc::new(41), false);
    assert!(cache.publish(pending, &pool));
    let (published, pixels) = cache.lookup(&probe(&effect, grid(), scene)).unwrap();
    assert_eq!(published, token);
    assert_eq!(*pixels, 41);
}

#[test]
fn preparation_releases_only_unselected_sealed_pixels_and_keeps_content_and_readers() {
    let pool = pool();
    let cache = Cache::<Arc<u32>>::default();
    let scene = Arc::new(Scene::new());
    let mut effects = [effect(), effect(), effect()];
    for (index, effect) in effects.iter_mut().enumerate() {
        effect.definition[0] = index as u8;
    }
    let mut tokens = Vec::new();
    for (index, effect) in effects.iter().enumerate() {
        let candidate = cache
            .prepare(probe(effect, grid(), scene.clone()), &pool)
            .unwrap();
        tokens.push(candidate.token);
        let pixels = Arc::new(index as u32);
        let pending = if index == 0 {
            candidate.finish(pixels)
        } else {
            let mut pending = candidate.observe();
            pending.materialize(pixels, false);
            pending
        };
        assert!(cache.publish(pending, &pool));
    }
    let reader = cache
        .lookup(&probe(&effects[1], grid(), scene.clone()))
        .unwrap()
        .1;
    let retired = Arc::downgrade(&reader);
    assert_eq!(cache.release_tail_outputs(|token| token == tokens[2]), 1);
    assert!(
        cache
            .lookup(&probe(&effects[1], grid(), scene.clone()))
            .is_none()
    );
    for (effect, token) in effects.iter().zip(&tokens) {
        assert_eq!(
            cache.lookup_content(&probe(effect, grid(), scene.clone())),
            Some(*token)
        );
    }
    assert_eq!(*reader, 1);
    drop(reader);
    assert!(retired.upgrade().is_none());
    assert_eq!(cache.release_tail_outputs(|_| false), 1);
    assert_eq!(
        cache.lookup(&probe(&effects[0], grid(), scene.clone())),
        Some((tokens[0], Arc::new(0)))
    );
    // A later spare allocation replaces sealed provenance for the same identity.
    let replacement = cache
        .prepare(probe(&effects[2], grid(), scene.clone()), &pool)
        .unwrap();
    assert!(cache.publish(replacement.finish(Arc::new(2)), &pool));
    assert_eq!(cache.release_tail_outputs(|_| false), 0);
    assert_eq!(
        cache.lookup(&probe(&effects[2], grid(), scene)).unwrap().0,
        tokens[2]
    );
}
