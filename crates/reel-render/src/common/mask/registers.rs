//! Reuse scalar storage after an instruction's last read, without rescheduling.
use super::{MaskOp, MaskProgram, NODES};

/// Allocation-free storage planning for the bounded portable mask subset.
/// Larger programs retain the WGPU generator's ordinary SSA representation.
pub(crate) struct RegisterPlan {
    destinations: [u8; NODES],
    registers: u8,
}

impl RegisterPlan {
    pub(crate) fn new(program: &MaskProgram) -> Option<Self> {
        let operations = program.operations();
        if operations.len() > NODES {
            return None;
        }
        let mut last_read: [u8; NODES] = std::array::from_fn(|i| i as u8);
        for (index, operation) in operations.iter().enumerate() {
            visit_reads(operation, |source| last_read[source as usize] = index as u8);
        }
        let mut result = Self {
            destinations: [0; NODES],
            registers: 0,
        };
        let mut occupied = 0_u32;
        for (index, operation) in operations.iter().enumerate() {
            // Every operand is evaluated before the destination is assigned.
            // A repeated input may expire twice; clearing the bit is idempotent.
            visit_reads(operation, |source| {
                if usize::from(last_read[source as usize]) == index {
                    occupied &= !(1 << result.destinations[source as usize]);
                }
            });
            let destination = (!occupied).trailing_zeros() as u8;
            debug_assert!(usize::from(destination) < NODES);
            result.destinations[index] = destination;
            result.registers = result.registers.max(destination + 1);
            if usize::from(last_read[index]) > index {
                occupied |= 1 << destination;
            }
        }
        Some(result)
    }

    pub(crate) fn register_count(&self) -> usize {
        usize::from(self.registers)
    }

    pub(crate) fn destination(&self, index: usize) -> u32 {
        u32::from(self.destinations[index])
    }

    /// Rewrite only node references. Constant bits, image/channel/repeat identity
    /// and the instruction sequence remain unchanged, including unused nodes.
    pub(crate) fn operation(&self, operation: &MaskOp) -> MaskOp {
        let read = |source: u32| self.destination(source as usize);
        match *operation {
            MaskOp::Sample {
                image,
                channel,
                coordinates,
                repeat,
            } => MaskOp::Sample {
                image,
                channel,
                coordinates: coordinates.map(read),
                repeat,
            },
            MaskOp::Add(a, b) => MaskOp::Add(read(a), read(b)),
            MaskOp::Subtract(a, b) => MaskOp::Subtract(read(a), read(b)),
            MaskOp::Multiply(a, b) => MaskOp::Multiply(read(a), read(b)),
            MaskOp::Pow(a, power) => MaskOp::Pow(read(a), power),
            MaskOp::Clamp(a, low, high) => MaskOp::Clamp(read(a), low, high),
            MaskOp::Smoothstep(a, b, x) => MaskOp::Smoothstep(read(a), read(b), read(x)),
            MaskOp::ImageThreshold { alpha, gray } => MaskOp::ImageThreshold {
                alpha: read(alpha),
                gray: read(gray),
            },
            _ => operation.clone(),
        }
    }
}

pub(super) fn visit_reads(operation: &MaskOp, mut visit: impl FnMut(u32)) {
    match *operation {
        MaskOp::Sample { coordinates, .. } => coordinates.into_iter().for_each(visit),
        MaskOp::Add(a, b)
        | MaskOp::Subtract(a, b)
        | MaskOp::Multiply(a, b)
        | MaskOp::ImageThreshold { alpha: a, gray: b } => {
            visit(a);
            visit(b);
        }
        MaskOp::Pow(a, _) | MaskOp::Clamp(a, _, _) => visit(a),
        MaskOp::Smoothstep(a, b, x) => {
            visit(a);
            visit(b);
            visit(x);
        }
        MaskOp::Constant(_) | MaskOp::Parameter(_) => {}
    }
}
