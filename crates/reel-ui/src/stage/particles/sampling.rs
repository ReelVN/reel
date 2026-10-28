//! Birth constants are retained separately from time-dependent analytic motion.
use super::{BudgetTicket, ParticleBatches, ParticleConfig, ParticleFrame, ParticleNode, Sprite};
use std::ops::Range;

#[derive(Clone, Copy, Default)]
pub(super) struct Birth {
    pub time: f64,
    pub lifetime: f64,
    pub offset: [f64; 2],
    pub velocity: [f64; 2],
    pub size: f64,
    pub angle: f64,
    pub spin: f64,
}
impl Birth {
    pub(super) fn sample(self, config: &ParticleConfig, frame: ParticleFrame, age: f64) -> Sprite {
        let progress = if self.lifetime == 0. {
            1.
        } else {
            (age / self.lifetime).clamp(0., 1.)
        };
        // Integrate dv/dt = acceleration - damping * velocity analytically.
        let (travel, accelerated) = if config.damping == 0. {
            (age, 0.5 * age * age)
        } else {
            let x = config.damping * age;
            if x.abs() < 1e-4 {
                (
                    age * (1. - x / 2. + x * x / 6.),
                    age * age * (0.5 - x / 6. + x * x / 24.),
                )
            } else {
                let travel = -(-x).exp_m1() / config.damping;
                (travel, (age - travel) / config.damping)
            }
        };
        let size = self.size + (config.size_end.unwrap_or(self.size) - self.size) * progress;
        Sprite {
            position: std::array::from_fn(|axis| {
                frame.origin[axis]
                    + self.offset[axis]
                    + self.velocity[axis] * travel
                    + config.acceleration[axis] * accelerated
            }),
            size: size
                * config
                    .size_curve
                    .as_ref()
                    .map_or(1., |curve| curve.sample(progress).max(0.)),
            opacity: config.opacity[0] + (config.opacity[1] - config.opacity[0]) * progress,
            rotation: self.angle + self.spin * age,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct CachedBirth {
    index: u64,
    birth: Birth,
}
struct Cache {
    source: ParticleBatches,
    ranges: Vec<Range<usize>>,
    births: Vec<CachedBirth>,
    _ticket: BudgetTicket,
}
const MAX_CACHE_BYTES: usize = 64 * 1024;

impl Cache {
    fn new(source: &ParticleBatches) -> Option<Box<Self>> {
        let storage = source.0.as_ref()?;
        let overhead = source
            .len()
            .checked_mul(size_of::<Range<usize>>())?
            .checked_add(size_of::<Self>())?;
        let maximum = MAX_CACHE_BYTES.checked_sub(overhead)? / size_of::<CachedBirth>();
        let needed = |config: &ParticleConfig| {
            (config.burst as f64 + (config.rate * config.max_lifetime()).ceil()).min(maximum as f64)
                as usize
        };
        let total: usize = source.iter().map(|emission| needed(&emission.config)).sum();
        let count = total.min(maximum);
        if count == 0 {
            return None;
        }
        let mut ticket = storage
            .pool
            .reserve((overhead + count * size_of::<CachedBirth>()) as u64)
            .ok()?;
        let mut ranges = Vec::new();
        ranges.try_reserve_exact(source.len()).ok()?;
        let mut births = Vec::new();
        births.try_reserve_exact(count).ok()?;
        ticket
            .resize(
                (size_of::<Self>()
                    + ranges.capacity() * size_of::<Range<usize>>()
                    + births.capacity() * size_of::<CachedBirth>()) as u64,
            )
            .ok()?;
        let (mut start, mut remaining) = (0, total);
        for emission in source.iter() {
            let need = needed(&emission.config);
            let slots = ((count - start) * need).checked_div(remaining).unwrap_or(0);
            ranges.push(start..start + slots);
            start += slots;
            remaining -= need;
        }
        births.resize(count, CachedBirth::default());
        Some(Box::new(Self {
            source: source.clone(),
            ranges,
            births,
            _ticket: ticket,
        }))
    }
}

/// Reuses deterministic birth data while sampling the current analytic motion.
/// Each sampler holds at most 64 KiB from the emission's CPU-output budget.
/// Additional particles use direct sampling. The rolling prefix retains its
/// entries across frames, including when the live population exceeds the cache.
/// A sampler belongs to one drawing consumer; snapshots store emission data.
#[derive(Default)]
pub struct ParticleSampler(Option<Box<Cache>>);
impl ParticleSampler {
    /// Sample one immutable, budgeted recording while retaining birth-cache reuse.
    pub fn record(
        &mut self,
        node: &ParticleNode,
    ) -> Result<crate::frame::SpriteInstances, crate::frame::BuildErrorKind> {
        let Some(storage) = node.batches.0.as_ref() else {
            return Ok(Default::default());
        };
        crate::frame::SpriteInstances::try_collect(&storage.pool, |emit| {
            self.for_each(node, |s| {
                if s.opacity > 0. && s.size > 0. {
                    emit(crate::frame::SpriteInstance {
                        position: s.position,
                        size: s.size,
                        rotation: s.rotation.to_radians(),
                        alpha: s.opacity as f32,
                    });
                }
            });
        })
    }

    /// Visit live sprites in emission and birth order. Clock jumps, pause, size
    /// scale and replacement emissions use the same formulas as `samples()`.
    pub fn for_each(&mut self, node: &ParticleNode, mut emit: impl FnMut(Sprite)) {
        if self
            .0
            .as_ref()
            .is_none_or(|cache| cache.source != node.batches)
        {
            self.0 = None;
            self.0 = Cache::new(&node.batches);
        }
        for (emission_index, emission) in node.batches.iter().enumerate() {
            let frame = emission.frame_at(node.time);
            let live = emission.config.live_range(frame);
            let first = *live.start();
            for index in live {
                let birth = if let Some(cache) = &mut self.0
                    && index - first < cache.ranges[emission_index].len() as u64
                {
                    let range = &cache.ranges[emission_index];
                    let slot =
                        &mut cache.births[range.start + (index % range.len() as u64) as usize];
                    if slot.index != index {
                        slot.index = index;
                        slot.birth =
                            emission
                                .config
                                .birth(frame, index, emission.config.lifetime_at(index));
                    }
                    slot.birth
                } else {
                    emission
                        .config
                        .birth(frame, index, emission.config.lifetime_at(index))
                };
                if frame.elapsed < birth.time + birth.lifetime {
                    let mut sprite = birth.sample(
                        &emission.config,
                        frame,
                        (frame.elapsed - birth.time).max(0.),
                    );
                    sprite.size *= node.size_scale;
                    emit(sprite);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
