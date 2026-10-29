use super::*;
use reel_core::budget::{BudgetCategory, QueueLimits};

fn pool() -> BudgetPool {
    BudgetPool::new(
        BudgetCategory::Temporary,
        QueueLimits {
            max_requests: 32,
            max_bytes: 65536,
        },
    )
}
fn source(scene: Arc<Scene>) -> Source {
    Source::Recording(Recording {
        owner: 1,
        scene,
        local: LocalId::None,
        image: ImageProjection {
            resource: ResourceHandle {
                registry: 1,
                slot: 2,
                generation: 3,
            },
            color: [1f32.to_bits(); 4],
            source_rect: Some([0, 0, 32f64.to_bits(), 32f64.to_bits()]),
            x_extend: Extend::Pad,
            y_extend: Extend::Pad,
            quality: ImageQuality::Medium,
            alpha: 1f32.to_bits(),
            transform: affine(Affine::IDENTITY),
            brush_transform: None,
            appearance: None,
        },
    })
}
fn effect() -> FrameEffect {
    FrameEffect {
        definition: [7; 32],
        shader: ResourceHandle {
            registry: 3,
            slot: 4,
            generation: 5,
        },
        values: vec![EffectValue::Float(0f32.to_bits())],
        images: vec![ResourceHandle {
            registry: 3,
            slot: 7,
            generation: 1,
        }],
        bounds: Rect::new(0., 0., 32., 32.),
        coverage_invert: false,
    }
}
fn grid() -> Grid {
    Grid {
        source_transform: affine(Affine::IDENTITY),
        effect_transform: affine(Affine::IDENTITY),
        source_size: [32, 32],
        source_storage: [32, 32],
        source_scissor: [0, 0, 32, 32],
        output_size: [32, 32],
        output_storage: [32, 32],
        scissor: [0, 0, 32, 32],
        quality: [0, 1],
    }
}
fn probe(effect: &FrameEffect, grid: Grid, scene: Arc<Scene>) -> Probe<'_> {
    Probe {
        source: source(scene),
        effect,
        grid,
    }
}
#[test]
fn image_projection_compares_actual_source_raster_and_appearance() {
    use reel_ui::frame::{
        ImageAlphaLayout, ImageAppearance, ImageColorAdjustment, ImageColorGradient,
    };
    let brush = ImageBrush {
        image: FrameImage {
            resource: ResourceHandle {
                registry: 1,
                slot: 2,
                generation: 3,
            },
            color: [1.; 4],
            source_rect: None,
            appearance: None,
        },
        sampler: Default::default(),
    };
    let original = ImageProjection::new(&brush, Affine::IDENTITY, None, Affine::IDENTITY);
    for offset in [(8.25, -11.5), (-32., 64.)] {
        let moved = Affine::translate(offset);
        assert!(original == ImageProjection::new(&brush, moved, None, moved.inverse()));
        assert!(original != ImageProjection::new(&brush, moved, None, Affine::IDENTITY));
    }
    let mut adjusted = brush;
    adjusted.image.appearance = Some(ImageAppearance {
        alpha_layout: ImageAlphaLayout::Native,
        gradient: Some(ImageColorGradient {
            colors: [[0.25, 0.5, 0.75, 1.]; 4],
            transform: Affine::IDENTITY,
        }),
        adjustment: ImageColorAdjustment {
            saturation: 0.5,
            ..Default::default()
        },
    });
    let projection = ImageProjection::new(&adjusted, Affine::IDENTITY, None, Affine::IDENTITY);
    assert!(projection != original);
    let translation = Affine::translate((8., 16.));
    let mut moved = adjusted;
    moved.image.appearance = moved.image.appearance.map(|a| a.transformed(translation));
    assert!(projection == ImageProjection::new(&moved, translation, None, translation.inverse()));
    for mutation in 0..4 {
        let mut changed = adjusted;
        let appearance = changed.image.appearance.as_mut().unwrap();
        match mutation {
            0 => appearance.adjustment.gain[1] = 0.8,
            1 => appearance.adjustment.offset[2] = 0.1,
            2 => appearance.alpha_layout = ImageAlphaLayout::TopBottom,
            _ => appearance.gradient.as_mut().unwrap().colors[2][0] = 0.9,
        }
        assert!(
            projection != ImageProjection::new(&changed, Affine::IDENTITY, None, Affine::IDENTITY)
        );
    }
}

#[test]
fn output_pixels_ignore_the_original_recording_transform_but_keep_the_effect_grid() {
    let pool = pool();
    let effect = effect();
    let key = Key::new(Probe::from_output(Token(1, 2), &effect, grid()), &pool).unwrap();
    let mut moved = grid();
    moved.source_transform = affine(Affine::translate((21., -7.)));
    assert!(key.matches(&Probe::from_output(Token(1, 2), &effect, moved)));
    moved.effect_transform = affine(Affine::translate((0.25, 0.)));
    assert!(!key.matches(&Probe::from_output(Token(1, 2), &effect, moved)));
}

#[test]
fn allocation_pressure_releases_optional_pixels_but_preserves_inflight_readers_and_family() {
    let pool = pool();
    let effect = effect();
    let scene = Arc::new(Scene::new());
    let cache = Cache::<Arc<u32>>::default();
    let output = Arc::new(73);
    let candidate = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    let token = candidate.token;
    assert!(cache.publish(candidate.finish(output.clone()), &pool));
    let (_, reader) = cache
        .lookup(&probe(&effect, grid(), scene.clone()))
        .unwrap();
    assert!(cache.reclaim_output());
    assert!(!cache.reclaim_output());
    assert_eq!(*reader, 73);
    assert_eq!(Arc::strong_count(&output), 2);
    assert!(
        cache
            .lookup(&probe(&effect, grid(), scene.clone()))
            .is_none()
    );
    let next = cache.prepare(probe(&effect, grid(), scene), &pool).unwrap();
    assert_eq!(next.token.1, token.1);
    assert_ne!(next.token.0, token.0);
    assert!(next.retain);
}

#[test]
fn key_preserves_recording_and_exact_dependency_bits() {
    let pool = pool();
    let scene = Arc::new(Scene::new());
    let effect = effect();
    let key = Key::new(
        Probe {
            source: source(scene.clone()),
            effect: &effect,
            grid: grid(),
        },
        &pool,
    )
    .unwrap();
    assert!(key.matches(&probe(&effect, grid(), scene.clone())));
    assert!(!key.matches(&probe(&effect, grid(), Arc::new(Scene::new()))));
    let mut changed = effect.clone();
    changed.values[0] = EffectValue::Float((-0.0f32).to_bits());
    assert!(!key.matches(&probe(&changed, grid(), scene.clone())));
    changed = effect.clone();
    changed.images[0].generation += 1;
    assert!(!key.matches(&probe(&changed, grid(), scene.clone())));
    changed = effect.clone();
    changed.shader.generation += 1;
    assert!(!key.matches(&probe(&changed, grid(), scene.clone())));
    changed = effect.clone();
    changed.bounds.x1 += 1.;
    assert!(!key.matches(&probe(&changed, grid(), scene.clone())));
    let mut changed_grid = grid();
    changed_grid.source_transform[4] = 1f64.to_bits();
    assert!(!key.matches(&probe(&effect, changed_grid, scene.clone())));
    changed_grid = grid();
    changed_grid.output_storage[0] += 1;
    assert!(!key.matches(&probe(&effect, changed_grid, scene.clone())));
    changed_grid = grid();
    changed_grid.source_scissor[2] -= 1;
    assert!(!key.matches(&probe(&effect, changed_grid, scene.clone())));
    changed_grid = grid();
    changed_grid.scissor[2] -= 1;
    assert!(!key.matches(&probe(&effect, changed_grid, scene.clone())));
    changed_grid = grid();
    changed_grid.quality[0] += 1;
    assert!(!key.matches(&probe(&effect, changed_grid, scene.clone())));
    let mut other_source = source(scene.clone());
    let Source::Recording(recording) = &mut other_source else {
        unreachable!()
    };
    recording.image.resource.generation += 1;
    assert!(!key.matches(&Probe {
        source: other_source,
        effect: &effect,
        grid: grid()
    }));
    assert!(pool.usage().bytes > 0);
    drop(key);
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
fn candidate_is_private_until_publish_and_output_tokens_never_repeat() {
    let pool = pool();
    let effect = effect();
    let scene = Arc::new(Scene::new());
    let cache = Cache::<Arc<()>>::default();
    let a = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap()
        .finish(Arc::new(()));
    let token = a.token;
    assert!(
        cache
            .lookup(&probe(&effect, grid(), scene.clone()))
            .is_none()
    );
    assert!(cache.publish(a, &pool));
    assert_eq!(
        cache
            .lookup(&probe(&effect, grid(), scene.clone()))
            .unwrap()
            .0,
        token
    );
    let output_key = Key::new(Probe::from_output(token, &effect, grid()), &pool).unwrap();
    assert!(output_key.matches(&Probe::from_output(token, &effect, grid())));
    assert!(!output_key.matches(&Probe::from_output(
        Token(token.0 + 1, token.1),
        &effect,
        grid()
    )));
    drop(output_key);
    let second = cache
        .prepare(Probe::from_output(token, &effect, grid()), &pool)
        .unwrap()
        .finish(Arc::new(()));
    let second_token = second.token;
    assert!(
        cache
            .lookup(&Probe::from_output(token, &effect, grid()))
            .is_none()
    );
    assert!(cache.publish(second, &pool));
    assert_eq!(
        cache
            .lookup(&Probe::from_output(token, &effect, grid()))
            .unwrap()
            .0,
        second_token
    );
    assert!(
        cache
            .lookup(&Probe::from_output(
                Token(token.0 + 1, token.1),
                &effect,
                grid()
            ))
            .is_none()
    );
    cache.clear();
    let b = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap()
        .finish(Arc::new(()));
    assert_ne!(token, b.token);
    assert!(cache.publish(b, &pool));
    let lease = cache.lookup(&probe(&effect, grid(), scene)).unwrap().1;
    cache.clear();
    assert_eq!(Arc::strong_count(&lease), 1);
    drop(lease);
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
#[ignore = "repeatable CPU-only effect key hit cost; does not execute GPU passes"]
fn effect_key_hot_hit_cost() {
    use std::{hint::black_box, time::Instant};
    let pool = pool();
    let effect = effect();
    let scene = Arc::new(Scene::new());
    let key = Key::new(probe(&effect, grid(), scene.clone()), &pool).unwrap();
    let retained = pool.usage().bytes;
    let started = Instant::now();
    let mut hits = 0;
    for _ in 0..100_000 {
        hits +=
            usize::from(black_box(&key).matches(&probe(black_box(&effect), grid(), scene.clone())));
    }
    println!(
        "effect_key hits={hits} iterations=100000 elapsed_ns={} retained_bytes={} additional_budget_bytes={}",
        started.elapsed().as_nanos(),
        retained,
        pool.usage().bytes - retained
    );
    assert_eq!(hits, 100_000);
    assert_eq!(pool.usage().bytes, retained);
    let cache = Cache::<Arc<()>>::default();
    let candidate = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    assert!(cache.publish(candidate.finish(Arc::new(())), &pool));
    let admitted = pool.usage().bytes;
    let started = Instant::now();
    let mut hits = 0;
    for _ in 0..100_000 {
        hits += usize::from(
            cache
                .lookup(&probe(black_box(&effect), grid(), scene.clone()))
                .is_some(),
        );
    }
    println!(
        "effect_cache hits={hits} iterations=100000 elapsed_ns={} cache_bytes={} additional_budget_bytes={}",
        started.elapsed().as_nanos(),
        admitted - retained,
        pool.usage().bytes - admitted
    );
    assert_eq!(hits, 100_000);
    assert_eq!(pool.usage().bytes, admitted);
}

#[test]
fn changing_parameters_keep_bounded_observations_until_dependencies_stabilize() {
    let pool = pool();
    let cache = Cache::<Arc<()>>::default();
    let second_effect = effect();
    let mut previous: Option<(Token, Token)> = None;
    for frame in 0..256 {
        // Simulate a freshly recorded source for the same effect node.
        let scene = Arc::new(Scene::new());
        let mut first_effect = effect();
        first_effect.values[0] = EffectValue::Float((frame as f32).to_bits());
        let first = cache
            .prepare(probe(&first_effect, grid(), scene.clone()), &pool)
            .unwrap();
        let first_token = first.token;
        if let Some((old_first, old_second)) = previous {
            assert_ne!(first_token.0, old_first.0);
            assert_eq!(first_token.1, old_first.1);
            assert_eq!(
                cache
                    .lookup(&Probe::from_output(old_first, &second_effect, grid()))
                    .is_some(),
                frame == 1,
            );
            assert_ne!(first_token, old_second);
        }
        assert_eq!(first.retain, frame == 0);
        let first = if first.retain {
            first.finish(Arc::new(()))
        } else {
            first.observe()
        };
        assert!(cache.publish(first, &pool));
        let second = cache
            .prepare(
                Probe::from_output(first_token, &second_effect, grid()),
                &pool,
            )
            .unwrap();
        let second_token = second.token;
        if let Some((_, old_second)) = previous {
            assert_eq!(second_token.1, old_second.1);
        }
        assert_eq!(second.retain, frame == 0);
        let second = if second.retain {
            second.finish(Arc::new(()))
        } else {
            second.observe()
        };
        assert!(cache.publish(second, &pool));
        assert_eq!(cache.test_stats().0, if frame == 0 { 2 } else { 0 });
        assert_eq!(cache.0.lock().unwrap().entries.len(), 2);
        assert_eq!(
            cache
                .lookup(&Probe::from_output(first_token, &second_effect, grid()))
                .is_some(),
            frame == 0
        );
        // Repeating the newly published identity permits pixel retention again.
        let settled = cache
            .prepare(probe(&first_effect, grid(), scene), &pool)
            .unwrap();
        assert!(settled.retain);
        drop(settled);
        previous = Some((first_token, second_token));
    }
    cache.clear();
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
fn changed_dependencies_wait_for_submission_then_restore_retention_without_releasing_readers() {
    let pool = pool();
    let mut effect = effect();
    let scene = Arc::new(Scene::new());
    let cache = Cache::<Arc<()>>::default();
    let output = Arc::new(());
    let flight = output.clone();
    let candidate = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    assert!(candidate.retain);
    assert!(cache.publish(candidate.finish(output), &pool));
    effect.values[0] = EffectValue::Float(1f32.to_bits());
    let cancelled = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    assert!(!cancelled.retain);
    drop(cancelled);
    let candidate = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    assert!(
        !candidate.retain,
        "abandoned work must not establish stability"
    );
    assert_eq!(Arc::strong_count(&flight), 2);
    assert!(cache.publish(candidate.observe(), &pool));
    assert_eq!(Arc::strong_count(&flight), 1);
    assert!(
        cache
            .lookup(&probe(&effect, grid(), scene.clone()))
            .is_none()
    );
    let settled = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    assert!(settled.retain);
    assert!(cache.publish(settled.finish(Arc::new(())), &pool));
    assert!(cache.lookup(&probe(&effect, grid(), scene)).is_some());
    for _ in 0..32 {
        cache.submitted();
    }
    assert_eq!(cache.test_stats().0, 0);
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
fn refused_optional_metadata_does_not_keep_a_zero_byte_ticket() {
    let pool = BudgetPool::new(
        BudgetCategory::Temporary,
        QueueLimits {
            max_requests: 1,
            max_bytes: 65536,
        },
    );
    let cache = Cache::<Arc<()>>::default();
    let effect = effect();
    let scene = Arc::new(Scene::new());
    let candidate = cache.prepare(probe(&effect, grid(), scene), &pool).unwrap();
    assert!(!cache.publish(candidate.finish(Arc::new(())), &pool));
    assert_eq!(pool.usage().bytes, 0);
    let ticket = pool.reserve(1).unwrap();
    drop(ticket);
}

#[test]
fn cold_results_reclaim_incrementally_without_invalidating_a_flight_owner() {
    let pool = pool();
    let effect = effect();
    let scene = Arc::new(Scene::new());
    let cache = Cache::<Arc<()>>::default();
    let output = Arc::new(());
    let flight = output.clone();
    let candidate = cache
        .prepare(probe(&effect, grid(), scene.clone()), &pool)
        .unwrap();
    assert!(cache.publish(candidate.finish(output), &pool));
    for _ in 0..31 {
        cache.submitted();
    }
    assert!(cache.lookup(&probe(&effect, grid(), scene)).is_some());
    for _ in 0..31 {
        cache.submitted();
    }
    assert_eq!(cache.test_stats().0, 1);
    cache.submitted();
    assert_eq!(cache.test_stats().0, 0);
    assert_eq!(Arc::strong_count(&flight), 1);
    assert_eq!(pool.usage().bytes, 0);
}
