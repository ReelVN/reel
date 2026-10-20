// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Backend-independent scalar coverage expressions. Operations are in DAG order.
use std::{
    hash::{Hash, Hasher},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum MaskChannel {
    Alpha,
    Gray,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum MaskOp {
    Constant(u32),
    Parameter(u8),
    Sample {
        image: u32,
        channel: MaskChannel,
        coordinates: [u32; 4],
        repeat: u8,
    },
    Add(u32, u32),
    Subtract(u32, u32),
    Multiply(u32, u32),
    Pow(u32, u32),
    Clamp(u32, u32, u32),
    Smoothstep(u32, u32, u32),
    /// Internal string shorthand. Construct through `MaskProgram::image`.
    ImageThreshold {
        alpha: u32,
        gray: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaskProgram {
    operations: Vec<MaskOp>,
    sources: Vec<Arc<str>>,
    image_threshold: bool,
    structure_hash: u64,
}

impl MaskProgram {
    /// Validate graph references and finite constants once, before publishing.
    pub fn new(operations: Vec<MaskOp>, sources: Vec<Arc<str>>) -> Result<Arc<Self>, &'static str> {
        if operations.is_empty()
            || operations.len() > u32::MAX as usize
            || sources.len() > u32::MAX as usize
            || sources.iter().any(|s| s.is_empty())
        {
            return Err("invalid mask expression storage");
        }
        for (index, operation) in operations.iter().enumerate() {
            let prior = |value: u32| (value as usize) < index;
            let finite = |bits| f32::from_bits(bits).is_finite();
            let valid = match *operation {
                MaskOp::Constant(bits) => finite(bits),
                MaskOp::Parameter(index) => index < 4,
                MaskOp::Sample {
                    image,
                    coordinates,
                    repeat,
                    ..
                } => {
                    (image as usize) < sources.len()
                        && repeat < 4
                        && coordinates.into_iter().all(prior)
                }
                MaskOp::Add(a, b) | MaskOp::Subtract(a, b) | MaskOp::Multiply(a, b) => {
                    prior(a) && prior(b)
                }
                MaskOp::ImageThreshold { .. } => false,
                MaskOp::Pow(a, power) => prior(a) && finite(power) && f32::from_bits(power) >= 0.,
                MaskOp::Clamp(a, min, max) => {
                    prior(a)
                        && finite(min)
                        && finite(max)
                        && f32::from_bits(min) <= f32::from_bits(max)
                }
                MaskOp::Smoothstep(a, b, x) => prior(a) && prior(b) && prior(x),
            };
            if !valid {
                return Err("invalid mask expression operation");
            }
        }
        let structure_hash = Self::hash(&operations, sources.len());
        Ok(Arc::new(Self {
            operations,
            sources,
            image_threshold: false,
            structure_hash,
        }))
    }
    fn hash(operations: &[MaskOp], images: usize) -> u64 {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        operations.hash(&mut hash);
        images.hash(&mut hash);
        hash.finish()
    }
    pub fn structure_hash(&self) -> u64 {
        self.structure_hash
    }
    pub fn operations(&self) -> &[MaskOp] {
        &self.operations
    }
    pub fn sources(&self) -> &[Arc<str>] {
        &self.sources
    }
    pub fn is_image_threshold(&self) -> bool {
        self.image_threshold
    }

    pub fn image(source: Arc<str>) -> Arc<Self> {
        let operations = vec![
            MaskOp::Constant(0),
            MaskOp::Constant(1_f32.to_bits()),
            MaskOp::Sample {
                image: 0,
                channel: MaskChannel::Alpha,
                coordinates: [0, 0, 1, 1],
                repeat: 0,
            },
            MaskOp::Sample {
                image: 0,
                channel: MaskChannel::Gray,
                coordinates: [0, 0, 1, 1],
                repeat: 0,
            },
            MaskOp::ImageThreshold { alpha: 2, gray: 3 },
        ];
        let structure_hash = Self::hash(&operations, 1);
        Arc::new(Self {
            operations,
            structure_hash,
            sources: vec![source],
            image_threshold: true,
        })
    }

    pub fn storage_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.operations.capacity() * std::mem::size_of::<MaskOp>()
            + self.sources.capacity() * std::mem::size_of::<Arc<str>>()
            + self.sources.iter().map(|s| s.len()).sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_admission_rejects_forward_edges_and_internal_shortcuts() {
        assert!(MaskProgram::new(vec![MaskOp::Add(0, 0)], vec![]).is_err());
        assert!(MaskProgram::new(vec![MaskOp::Parameter(4)], vec![]).is_err());
        assert!(
            MaskProgram::new(
                vec![
                    MaskOp::Constant(0),
                    MaskOp::ImageThreshold { alpha: 0, gray: 0 }
                ],
                vec![]
            )
            .is_err()
        );
    }
}
