//! Compile-time proof of a fixed logical-pixel source neighborhood.
use super::*;
use reel_ui::resources::EffectSourceFootprint;

#[derive(Clone, Copy)]
enum Term {
    Unknown,
    Constant(f32),
    Uv(u8),
    Size(u8),
    Offset(u8, f32),
    Coordinate(u8, f32),
}
type Value = [Term; 4];

/// Prove every reachable subtree read is `uv + constant / original_size`.
/// Arbitrary color arithmetic and branch unions remain valid; parameter-driven,
/// sampled or nonlinear coordinates preserve the original full-domain path.
pub fn source_footprint(program: &EffectProgram) -> Option<EffectSourceFootprint> {
    if program.nodes()[program.output().0 as usize].ty != EffectType::Vec4 {
        return None;
    }
    let mut live = vec![false; program.nodes().len()];
    live[program.output().0 as usize] = true;
    for index in (0..live.len()).rev() {
        if !live[index] {
            continue;
        }
        let op = &program.nodes()[index].op;
        for id in dependencies(op) {
            live[id.0 as usize] = true;
        }
    }
    let mut values: Vec<Value> = Vec::with_capacity(live.len());
    let mut halo = [0; 4];
    let mut sampled = false;
    for (index, node) in program.nodes().iter().enumerate() {
        let mut value = [Term::Unknown; 4];
        if live[index] {
            match node.op {
                EffectOp::Constant(constant) => {
                    let bits: &[u32] = match &constant {
                        EffectValue::Float(v) => std::slice::from_ref(v),
                        EffectValue::Vec2(v) => v,
                        EffectValue::Vec3(v) => v,
                        EffectValue::Vec4(v) => v,
                        EffectValue::Bool(_) => &[],
                    };
                    for (slot, &bits) in value.iter_mut().zip(bits) {
                        *slot = Term::Constant(f32::from_bits(bits));
                    }
                }
                EffectOp::Input(EffectInput::Uv) => {
                    value[0] = Term::Uv(0);
                    value[1] = Term::Uv(1);
                }
                EffectOp::Input(EffectInput::Size) => {
                    value[0] = Term::Size(0);
                    value[1] = Term::Size(1);
                }
                EffectOp::Vec2(ids) => {
                    for (slot, id) in value.iter_mut().zip(ids) {
                        *slot = values[id.0 as usize][0];
                    }
                }
                EffectOp::Vec3(ids) => {
                    for (slot, id) in value.iter_mut().zip(ids) {
                        *slot = values[id.0 as usize][0];
                    }
                }
                EffectOp::Vec4(ids) => {
                    for (slot, id) in value.iter_mut().zip(ids) {
                        *slot = values[id.0 as usize][0];
                    }
                }
                EffectOp::Extract { value: id, lane } => {
                    value[0] = values[id.0 as usize][lane as usize]
                }
                EffectOp::Binary { op, left, right } => {
                    value = std::array::from_fn(|lane| {
                        binary(
                            op,
                            values[left.0 as usize][lane],
                            values[right.0 as usize][lane],
                        )
                    });
                }
                EffectOp::SourceSample { uv } => {
                    sampled = true;
                    for (axis, &term) in values[uv.0 as usize][..2].iter().enumerate() {
                        let offset = match term {
                            Term::Uv(a) if a == axis as u8 => 0.,
                            Term::Coordinate(a, delta) if a == axis as u8 => delta,
                            _ => return None,
                        };
                        let radius = f64::from(offset).abs().ceil();
                        if !radius.is_finite() || radius > f64::from(u32::MAX) {
                            return None;
                        }
                        let side = axis + if offset < 0. { 0 } else { 2 };
                        halo[side] = halo[side].max(radius as u32);
                    }
                }
                _ => {}
            }
        }
        values.push(value);
    }
    sampled.then_some(EffectSourceFootprint { logical_halo: halo })
}

fn binary(op: EffectBinary, left: Term, right: Term) -> Term {
    use EffectBinary::{Add, Divide, Multiply, Subtract};
    match (op, left, right) {
        (Divide, Term::Constant(value), Term::Size(axis)) => Term::Offset(axis, value),
        (Add, Term::Uv(a), Term::Offset(b, delta)) | (Add, Term::Offset(b, delta), Term::Uv(a))
            if a == b =>
        {
            Term::Coordinate(a, delta)
        }
        (Subtract, Term::Uv(a), Term::Offset(b, delta)) if a == b => Term::Coordinate(a, -delta),
        (op, Term::Constant(a), Term::Constant(b)) => {
            let exact = match op {
                Add => f64::from(a) + f64::from(b),
                Subtract => f64::from(a) - f64::from(b),
                Multiply => f64::from(a) * f64::from(b),
                Divide => f64::from(a) / f64::from(b),
                _ => return Term::Unknown,
            };
            let result = exact as f32;
            if result.is_finite() && f64::from(result) == exact {
                Term::Constant(result)
            } else {
                Term::Unknown
            }
        }
        _ => Term::Unknown,
    }
}
