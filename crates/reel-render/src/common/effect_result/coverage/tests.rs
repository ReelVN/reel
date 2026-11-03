use super::super::tests::{effect, grid, pool, probe};
use super::*;

fn halo() -> Option<EffectSourceFootprint> {
    Some(EffectSourceFootprint {
        logical_halo: [2; 4],
    })
}
fn cropped_grid(effect: &FrameEffect) -> Grid {
    let mut grid = grid().color_context(effect);
    grid.scissor = [10, 10, 8, 8];
    grid
}

#[test]
fn proved_subset_reads_existing_pixels_but_fresh_outputs_keep_exact_identity() {
    let effect = effect();
    let scene = Arc::new(Scene::new());
    let pool = pool();
    let cache = Cache::<Arc<u32>>::default();
    let broad = grid().color_context(&effect);
    let initial = cache
        .prepare(probe(&effect, broad, scene.clone()), &pool)
        .unwrap();
    let original = initial.token;
    assert!(cache.publish(initial.finish(Arc::new(71)), &pool));
    let mut narrow = cropped_grid(&effect);
    let demand = probe(&effect, narrow, scene.clone())
        .with_source_demand(halo())
        .required
        .unwrap();
    narrow.source_scissor = [demand.x, demand.y, demand.width, demand.height];
    let plain = probe(&effect, narrow, scene.clone());
    assert!(
        cache.lookup(&plain).is_none(),
        "ordinary queries retain exact scissors"
    );
    let proved = plain.with_source_demand(halo());
    assert_eq!(cache.lookup_content(&proved), Some(original));
    assert_eq!(
        cache.lookup(&proved).map(|(token, value)| (token, *value)),
        Some((original, 71))
    );
    let fresh = cache.prepare(proved, &pool).unwrap();
    assert_ne!(
        fresh.token, original,
        "a narrow fresh raster has different outside pixels"
    );
    assert!(
        !fresh.retain,
        "a coverage read does not bypass exact warming"
    );
}

#[test]
fn coverage_requires_both_output_pixels_and_every_source_halo_texel() {
    let effect = effect();
    let scene = Arc::new(Scene::new());
    let pool = pool();
    let narrow = cropped_grid(&effect);
    let wanted = probe(&effect, narrow, scene.clone()).with_source_demand(halo());
    let required = wanted.required.unwrap();
    assert!(
        required.width > narrow.scissor[2],
        "blur and bilinear neighbors extend demand"
    );
    let mut stored = grid().color_context(&effect);
    stored.source_scissor = narrow.scissor;
    let key = Key::new(probe(&effect, stored, scene.clone()), &pool).unwrap();
    assert!(
        !key.covers(&wanted),
        "visible output alone does not prove halo pixels"
    );
    stored.source_scissor = [0, 0, 32, 32];
    stored.scissor = [11, 10, 7, 8];
    let key = Key::new(probe(&effect, stored, scene.clone()), &pool).unwrap();
    assert!(
        !key.covers(&wanted),
        "newly exposed output must be rendered"
    );
    let mut missing_halo = narrow;
    missing_halo.source_scissor = narrow.scissor;
    assert!(
        probe(&effect, missing_halo, scene)
            .with_source_demand(halo())
            .required
            .is_none()
    );
}

#[test]
fn semantic_bounds_context_and_shader_parameters_remain_exact() {
    let effect = effect();
    let scene = Arc::new(Scene::new());
    let pool = pool();
    let key = Key::new(
        probe(&effect, grid().color_context(&effect), scene.clone()),
        &pool,
    )
    .unwrap();
    let narrow = cropped_grid(&effect);
    let mut bounded = probe(&effect, narrow, scene.clone());
    let Source::Recording(recording) = &mut bounded.source else {
        unreachable!()
    };
    recording.bounds = Scissor {
        x: 0,
        y: 0,
        width: 20,
        height: 32,
    };
    assert!(!key.covers(&bounded.with_source_demand(halo())));
    let mut changed = effect.clone();
    changed.values[0] = EffectValue::Float(1f32.to_bits());
    assert!(!key.covers(&probe(&changed, narrow, scene.clone()).with_source_demand(halo())));
    let mut moved = narrow;
    moved.effect_context = context::ordinary(
        &effect,
        Affine::translate((0.25, 0.)),
        [32, 32],
        [32, 32],
        [32, 32],
    );
    assert!(!key.covers(&probe(&effect, moved, scene.clone()).with_source_demand(halo())));
    assert!(!key.covers(&probe(&effect, narrow, scene.clone()).with_source_demand(None)));
    let mut scalar = narrow;
    scalar.effect_context = None;
    assert!(
        probe(&effect, scalar, scene)
            .with_source_demand(halo())
            .required
            .is_none()
    );
}

#[test]
fn equal_wrapper_bounds_keep_their_initialized_transparent_halo() {
    let effect = effect();
    let scene = Arc::new(Scene::new());
    let pool = pool();
    let bounds = Scissor {
        x: 10,
        y: 10,
        width: 8,
        height: 8,
    };
    let source = |mut grid: Grid| {
        grid.source_scissor = [bounds.x, bounds.y, bounds.width, bounds.height];
        let mut value = probe(&effect, grid, scene.clone());
        let Source::Recording(recording) = &mut value.source else {
            unreachable!()
        };
        recording.bounds = bounds;
        value
    };
    let key = Key::new(source(grid().color_context(&effect)), &pool).unwrap();
    let wanted = source(cropped_grid(&effect)).with_source_demand(halo());
    assert_eq!(wanted.required, Some(bounds));
    assert!(key.covers(&wanted));
}
