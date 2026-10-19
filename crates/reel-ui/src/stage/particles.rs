// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Analytic emission batches sharing one sprite and coordinate space. Clock
//! updates reuse immutable batch storage; old views retain their budget lease.
use reel_core::{
    budget::{BudgetPool, BudgetTicket},
    result::{RejectCode, RejectReason},
};
use std::{ops::RangeInclusive, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ParticleConfig {
    pub rate: f64,
    pub burst: u64,
    pub lifetime: f64,
    pub lifetime_spread: f64,
    pub seed: u64,
    pub velocity: [f64; 2],
    pub spread: [f64; 2],
    pub acceleration: [f64; 2],
    pub size: f64,
    pub size_spread: f64,
    pub size_end: Option<f64>,
    pub opacity: [f64; 2],
    pub spin: f64,
    pub spin_spread: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ParticleFrame {
    pub origin: [f64; 2],
    pub elapsed: f64,
    pub duration: Option<f64>,
    pub extent: [f64; 2],
}

/// One immutable emission; elapsed time is based on the shared node clock.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ParticleEmission {
    pub sequence: u64,
    pub config: ParticleConfig,
    pub frame: ParticleFrame,
    pub started: f64,
    pub paused: bool,
}
impl ParticleEmission {
    pub fn frame_at(&self, time: f64) -> ParticleFrame {
        ParticleFrame {
            elapsed: self.frame.elapsed + if self.paused { 0. } else { time - self.started },
            ..self.frame
        }
    }
    pub fn samples(&self, time: f64) -> impl Iterator<Item = Sprite> + '_ {
        let frame = self.frame_at(time);
        self.config.live_range(frame).filter_map(move |index| {
            let birth = self.config.birth_time(index);
            let lifetime = self.config.lifetime_at(index);
            (frame.elapsed < birth + lifetime).then(|| {
                self.config
                    .sample_at(frame, index, (frame.elapsed - birth).max(0.), lifetime)
            })
        })
    }
}

#[derive(Debug)]
struct BatchStorage {
    // Data drops before the reservation, including when the last view is on
    // another thread or outlives its originating VM.
    entries: Vec<ParticleEmission>,
    _ticket: BudgetTicket,
}
#[derive(Clone, Debug, Default)]
pub struct ParticleBatches(Option<Arc<BatchStorage>>);
impl PartialEq for ParticleBatches {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        }
    }
}
impl std::ops::Deref for ParticleBatches {
    type Target = [ParticleEmission];
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().map_or(&[], |storage| &storage.entries)
    }
}
impl ParticleBatches {
    pub fn try_new(
        pool: &BudgetPool,
        count: usize,
        entries: impl IntoIterator<Item = ParticleEmission>,
    ) -> Result<Self, RejectReason> {
        if count == 0 {
            return Ok(Self::default());
        }
        let overhead = size_of::<BatchStorage>() + 2 * size_of::<usize>();
        let bytes = count
            .checked_mul(size_of::<ParticleEmission>())
            .and_then(|n| n.checked_add(overhead))
            .ok_or_else(|| {
                RejectReason::new(RejectCode::OutOfMemory, "particle projection size")
            })?;
        let mut ticket = pool.reserve(bytes as u64)?;
        let mut data = Vec::new();
        data.try_reserve_exact(count).map_err(|_| {
            RejectReason::new(RejectCode::OutOfMemory, "particle projection allocation")
        })?;
        ticket.resize((data.capacity() * size_of::<ParticleEmission>() + overhead) as u64)?;
        data.extend(entries);
        assert_eq!(data.len(), count, "trusted particle projection length");
        Ok(Self(Some(Arc::new(BatchStorage {
            entries: data,
            _ticket: ticket,
        }))))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParticleNode {
    pub source: String,
    pub origin: Option<String>,
    pub seed: u64,
    pub batches: ParticleBatches,
    pub time: f64,
}
impl ParticleNode {
    pub fn samples(&self) -> impl Iterator<Item = Sprite> + '_ {
        self.batches
            .iter()
            .flat_map(|batch| batch.samples(self.time))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sprite {
    pub position: [f64; 2],
    pub size: f64,
    pub opacity: f64,
    pub rotation: f64,
}

impl ParticleConfig {
    pub fn live_range(&self, frame: ParticleFrame) -> RangeInclusive<u64> {
        let last_time = frame.duration.unwrap_or(frame.elapsed).min(frame.elapsed);
        let last = self.last_index(last_time);
        let first = if frame.elapsed < self.max_lifetime() {
            1
        } else {
            self.last_index(frame.elapsed - self.max_lifetime())
                .saturating_add(1)
        };
        first..=last
    }

    pub fn max_lifetime(&self) -> f64 {
        self.lifetime + self.lifetime_spread
    }

    pub fn last_index(&self, elapsed: f64) -> u64 {
        self.burst
            .saturating_add((elapsed * self.rate).floor() as u64)
    }

    pub fn birth_time(&self, index: u64) -> f64 {
        if index <= self.burst {
            0.
        } else {
            (index - self.burst) as f64 / self.rate
        }
    }

    pub fn lifetime_at(&self, index: u64) -> f64 {
        if self.lifetime_spread == 0. {
            self.lifetime
        } else {
            self.lifetime + (2. * self.random(index, 6) - 1.) * self.lifetime_spread
        }
    }

    fn random(&self, index: u64, lane: u64) -> f64 {
        // Fixed SplitMix64 finalizer maps (seed, birth index, lane) to samples.
        // Counter addressing permits random access after skipped frames without
        // replaying an RNG or retaining a particle array. Lanes 0..5 remain
        // birth x/y, velocity x/y, size, angular velocity; lane 6 is lifetime.
        let mut z = self
            .seed
            .wrapping_add(index.wrapping_mul(0x9e3779b97f4a7c15))
            .wrapping_add(lane.wrapping_mul(0xd1b54a32d192ed03));
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        ((z ^ (z >> 31)) >> 11) as f64 * (1. / 9_007_199_254_740_992.)
    }

    pub fn sample(&self, frame: ParticleFrame, index: u64) -> Sprite {
        self.sample_at(
            frame,
            index,
            (frame.elapsed - self.birth_time(index)).max(0.),
            self.lifetime_at(index),
        )
    }

    fn sample_at(&self, frame: ParticleFrame, index: u64, age: f64, lifetime: f64) -> Sprite {
        let random = |lane| self.random(index, lane);
        let progress = if lifetime == 0. {
            1.
        } else {
            (age / lifetime).clamp(0., 1.)
        };
        let size = self.size + (2. * random(4) - 1.) * self.size_spread;
        Sprite {
            position: std::array::from_fn(|axis| {
                frame.origin[axis]
                    + random(axis as u64) * frame.extent[axis]
                    + (self.velocity[axis]
                        + (2. * random(axis as u64 + 2) - 1.) * self.spread[axis])
                        * age
                    + 0.5 * self.acceleration[axis] * age * age
            }),
            size: size + (self.size_end.unwrap_or(size) - size) * progress,
            opacity: self.opacity[0] + (self.opacity[1] - self.opacity[0]) * progress,
            rotation: (self.spin + (2. * random(5) - 1.) * self.spin_spread) * age,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn particle_birth_boundaries_seed_and_analytic_motion() {
        let config = ParticleConfig {
            rate: 4.,
            burst: 0,
            lifetime: 2.,
            lifetime_spread: 0.,
            seed: 42,
            velocity: [10., 20.],
            spread: [0., 0.],
            acceleration: [2., 4.],
            size: 8.,
            size_spread: 0.,
            size_end: None,
            opacity: [1., 0.],
            spin: 30.,
            spin_spread: 0.,
        };
        let mut frame = ParticleFrame {
            origin: [0., 0.],
            elapsed: 0.,
            duration: Some(1.),
            extent: [0., 0.],
        };
        assert_eq!(config.live_range(frame).count(), 0);
        frame.elapsed = 0.25;
        assert_eq!(config.live_range(frame), 1..=1);
        frame.elapsed = 1.25;
        assert_eq!(config.live_range(frame), 1..=4);
        let sprite = config.sample(frame, 1);
        assert_eq!(
            sprite,
            Sprite {
                position: [11., 22.],
                size: 8.,
                opacity: 0.5,
                rotation: 30.
            }
        );
        frame.origin = [-2.5, 12.];
        let translated = config.sample(frame, 1);
        assert_eq!(translated.position, [8.5, 34.]);
        frame.origin = [0., 0.];
        frame.elapsed = 2.25;
        assert_eq!(config.live_range(frame), 2..=4);
        frame.elapsed = 3.;
        assert_eq!(config.live_range(frame).count(), 0);
        frame.duration = None;
        frame.elapsed = 1_000_000.;
        assert_eq!(config.live_range(frame).count(), 8);
        frame.extent = [100., 100.];
        let mut other = config;
        other.seed = 43;
        assert_ne!(
            config.sample(frame, 4_000_000),
            other.sample(frame, 4_000_000)
        );
    }
}
