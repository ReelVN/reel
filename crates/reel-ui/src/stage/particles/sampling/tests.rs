use super::*;
use crate::stage::particles::ParticleEmission;
use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};

fn pool(bytes: u64) -> BudgetPool {
    BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 128,
            max_bytes: bytes,
        },
    )
}
fn emission(sequence: u64, burst: u64, rate: f64) -> ParticleEmission {
    ParticleEmission {
        sequence,
        started: 0.,
        paused: false,
        frame: ParticleFrame {
            origin: [-20., 15.],
            extent: [960., 544.],
            elapsed: 0.,
            duration: None,
        },
        config: ParticleConfig {
            rate,
            burst,
            lifetime: 2.,
            lifetime_spread: 0.6,
            seed: sequence,
            velocity: [12., 15.],
            spread: [50., 40.],
            acceleration: [0., 12.],
            size: 8.,
            size_spread: 4.,
            size_end: Some(1.),
            size_curve: None,
            angle: 25.,
            angle_spread: 180.,
            radial_speed: 32.,
            radial_spread: 12.,
            direction: 0.,
            direction_spread: 180.,
            damping: 0.4,
            opacity: [1., 0.],
            spin: 20.,
            spin_spread: 35.,
        },
    }
}
fn node(pool: &BudgetPool, emissions: &[ParticleEmission]) -> ParticleNode {
    ParticleNode {
        source: "sprite.png".into(),
        origin: None,
        seed: 0,
        size_scale: 1.,
        time: 0.,
        batches: ParticleBatches::try_new(pool, emissions.len(), emissions.iter().cloned())
            .unwrap(),
    }
}
fn compare(sampler: &mut ParticleSampler, node: &ParticleNode) {
    let expected: Vec<_> = node.samples().collect();
    let mut actual = Vec::new();
    sampler.for_each(node, |sprite| actual.push(sprite));
    assert_eq!(actual, expected);
}

#[test]
fn cached_births_preserve_motion_order_jumps_pause_and_replacements() {
    let pool = pool(1 << 20);
    let emissions = [emission(1, 200, 180.), emission(2, 150, 200.)];
    let mut node = node(&pool, &emissions);
    let mut sampler = ParticleSampler::default();
    let base = pool.usage().bytes;
    for time in [0., 0.016, 0.5, 1.5, 2.5, 3.5, 1e6, 0.1] {
        node.time = time;
        node.size_scale = 2.;
        compare(&mut sampler, &node);
        assert!(pool.usage().bytes - base <= MAX_CACHE_BYTES as u64);
    }
    let mut paused = emissions[0].clone();
    paused.paused = true;
    paused.frame.elapsed = 0.4;
    paused.config.seed = 9;
    paused.frame.extent = [42., 13.];
    node.batches = ParticleBatches::try_new(&pool, 1, [paused]).unwrap();
    compare(&mut sampler, &node);
    node.time += 1e6;
    compare(&mut sampler, &node);
    drop(node);
    drop(sampler);
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
fn oversize_populations_keep_a_rolling_prefix_and_budget_pressure_uses_direct_sampling() {
    let pool = pool(1 << 20);
    let mut node = node(&pool, &[emission(1, 0, 1_000.)]);
    let mut sampler = ParticleSampler::default();
    node.time = 4.;
    compare(&mut sampler, &node);
    let cache = sampler.0.as_ref().unwrap();
    let before: Vec<_> = cache.births.iter().map(|b| b.index).collect();
    node.time += 0.016;
    compare(&mut sampler, &node);
    let cache = sampler.0.as_ref().unwrap();
    let retained = before
        .iter()
        .zip(&cache.births)
        .filter(|(old, new)| **old == new.index)
        .count();
    assert!(retained >= cache.births.len() - 17);
    assert!(cache._ticket.bytes() <= MAX_CACHE_BYTES as u64);
    drop(sampler);
    let spare = (1 << 20) - pool.usage().bytes;
    let _pressure = pool.reserve(spare).unwrap();
    let mut sampler = ParticleSampler::default();
    compare(&mut sampler, &node);
    assert!(sampler.0.is_none());
}
