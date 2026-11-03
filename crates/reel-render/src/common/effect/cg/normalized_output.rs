//! Conservative component ranges for the ordinary GXM output selector.
//!
//! GXM SourceSample reads an already rasterized UNORM subtree (RGBA8 or the
//! replicated coverage channel), so each sampled component is finite in [0, 1]
//! with positive zero. External textures, live numeric parameters and logical
//! coordinates have no such range contract and remain unknown. Only the output
//! proof is consumed; author operations and their evaluation order stay intact.

use super::*;

#[derive(Clone, Copy)]
struct Range {
    low: f32,
    high: f32,
}
impl Range {
    fn new(low: f32, high: f32) -> Option<Self> {
        if !low.is_finite()
            || !high.is_finite()
            || low.is_sign_negative()
            || high.is_sign_negative()
            || low > high
        {
            return None;
        }
        Some(Self {
            // A shader target may flush subnormals to positive zero. Keeping
            // zero in this range also prevents proving such a divisor positive.
            low: if low.is_subnormal() { 0.0 } else { low },
            high,
        })
    }
    fn binary(self, op: EffectBinary, right: Self) -> Option<Self> {
        let (low, high) = match op {
            EffectBinary::Add => (self.low + right.low, self.high + right.high),
            EffectBinary::Multiply => (self.low * right.low, self.high * right.high),
            EffectBinary::Divide if right.low > 0.0 => {
                (self.low / right.high, self.high / right.low)
            }
            EffectBinary::Min => (self.low.min(right.low), self.high.min(right.high)),
            EffectBinary::Max => (self.low.max(right.low), self.high.max(right.high)),
            _ => return None,
        };
        // Each endpoint uses one original f32 operation, without reassociation
        // or wider arithmetic. Reject overflow before any later clamp can hide it.
        Self::new(low, high)
    }
    fn clamp(self, minimum: Self, maximum: Self) -> Option<Self> {
        if minimum.high > maximum.low {
            return None;
        }
        Self::new(
            self.low.clamp(minimum.low, maximum.low),
            self.high.clamp(minimum.high, maximum.high),
        )
    }
}

#[derive(Clone, Copy)]
struct Bounds {
    lanes: [Range; 4],
    count: usize,
}
impl Bounds {
    fn constant(value: EffectValue) -> Option<Self> {
        let (bits, count) = match value {
            EffectValue::Bool(_) => return None,
            EffectValue::Float(a) => ([a; 4], 1),
            EffectValue::Vec2([a, b]) => ([a, b, a, a], 2),
            EffectValue::Vec3([a, b, c]) => ([a, b, c, a], 3),
            EffectValue::Vec4(a) => (a, 4),
        };
        let mut output = Self::sample();
        output.count = count;
        for (lane, bits) in output.lanes.iter_mut().zip(bits).take(count) {
            let value = f32::from_bits(bits);
            *lane = Range::new(value, value)?;
        }
        Some(output)
    }
    fn sample() -> Self {
        Self {
            lanes: [Range {
                low: 0.0,
                high: 1.0,
            }; 4],
            count: 4,
        }
    }
    fn vector(nodes: &[Option<Self>], components: &[EffectNodeId]) -> Option<Self> {
        let mut output = Self::sample();
        output.count = components.len();
        for (lane, id) in output.lanes.iter_mut().zip(components) {
            *lane = nodes[id.0 as usize]?.lanes[0];
        }
        Some(output)
    }
    fn binary(mut self, op: EffectBinary, right: Self) -> Option<Self> {
        for index in 0..self.count {
            self.lanes[index] = self.lanes[index].binary(op, right.lanes[index])?;
        }
        Some(self)
    }
    fn clamp(mut self, minimum: Self, maximum: Self) -> Option<Self> {
        for index in 0..self.count {
            self.lanes[index] =
                self.lanes[index].clamp(minimum.lanes[index], maximum.lanes[index])?;
        }
        Some(self)
    }
    fn union(mut self, other: Self) -> Option<Self> {
        for index in 0..self.count {
            self.lanes[index] = Range::new(
                self.lanes[index].low.min(other.lanes[index].low),
                self.lanes[index].high.max(other.lanes[index].high),
            )?;
        }
        Some(self)
    }
}

/// Prove all output lanes finite in [0, 1], including positive-zero provenance.
/// This accepts arbitrary static nonnegative convolution sizes/weights when
/// their actual ordered f32 bounds fit. Unknown nodes affect only their users.
pub(super) fn proven(program: &EffectProgram) -> bool {
    if program.nodes()[program.output().0 as usize].ty != EffectType::Vec4 {
        return false;
    }
    let mut bounds: Vec<Option<Bounds>> = Vec::with_capacity(program.nodes().len());
    for node in program.nodes() {
        let get = |id: EffectNodeId| bounds[id.0 as usize];
        let value = match node.op {
            EffectOp::Constant(value) => Bounds::constant(value),
            EffectOp::SourceSample { .. } => Some(Bounds::sample()),
            EffectOp::Vec2(ids) => Bounds::vector(&bounds, &ids),
            EffectOp::Vec3(ids) => Bounds::vector(&bounds, &ids),
            EffectOp::Vec4(ids) => Bounds::vector(&bounds, &ids),
            EffectOp::Extract { value, lane } => get(value).map(|value| Bounds {
                lanes: [value.lanes[lane as usize]; 4],
                count: 1,
            }),
            EffectOp::Binary { op, left, right } => {
                get(left).zip(get(right)).and_then(|(a, b)| a.binary(op, b))
            }
            EffectOp::Ternary {
                op: EffectTernary::Clamp,
                a,
                b,
                c,
            } => get(a)
                .zip(get(b))
                .zip(get(c))
                .and_then(|((a, b), c)| a.clamp(b, c)),
            EffectOp::Select {
                when_true,
                when_false,
                ..
            } => get(when_true)
                .zip(get(when_false))
                .and_then(|(a, b)| a.union(b)),
            _ => None,
        };
        bounds.push(value);
    }
    bounds[program.output().0 as usize]
        .is_some_and(|output| output.lanes.iter().all(|range| range.high <= 1.0))
}
