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
    Source {
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
        },
    }
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
    changed_grid.scissor[2] -= 1;
    assert!(!key.matches(&probe(&effect, changed_grid, scene.clone())));
    changed_grid = grid();
    changed_grid.quality[0] += 1;
    assert!(!key.matches(&probe(&effect, changed_grid, scene.clone())));
    let mut other_source = source(scene.clone());
    other_source.image.resource.generation += 1;
    assert!(!key.matches(&Probe {
        source: other_source,
        effect: &effect,
        grid: grid()
    }));
    assert!(pool.usage().bytes > 0);
    drop(key);
    assert_eq!(pool.usage().bytes, 0);
}
