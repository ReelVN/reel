//! Identical authored mask input for the dynamic and offline-compiled probes.
use reel_ui::stage::{MaskChannel, MaskOp, MaskProgram};
use std::sync::Arc;

/// Stable fixture identity; both the packaged envelope and its instance use it.
pub const STATIC_DEFINITION: [u8; 32] = [0x74; 32];

pub fn program(case: &str) -> Result<Arc<MaskProgram>, String> {
    if case == "mask_threshold" {
        return Ok(MaskProgram::image("probe-mask".into()));
    }
    let mut operations = vec![
        MaskOp::Constant(0),
        MaskOp::Constant(1_f32.to_bits()),
        MaskOp::Parameter(0),
        MaskOp::Parameter(2),
        MaskOp::Sample {
            image: 0,
            channel: MaskChannel::Gray,
            coordinates: [3, 0, 1, 1],
            repeat: 1,
        },
        MaskOp::Subtract(2, 4),
        MaskOp::Constant(3_f32.to_bits()),
        MaskOp::Multiply(5, 6),
        MaskOp::Add(7, 1),
        MaskOp::Clamp(8, 0_f32.to_bits(), 1_f32.to_bits()),
    ];
    if matches!(case, "mask_dag" | "mask_static") {
        operations.push(MaskOp::Sample {
            image: 0,
            channel: MaskChannel::Alpha,
            coordinates: [0, 0, 1, 1],
            repeat: 0,
        });
        operations.push(MaskOp::Multiply(9, 10));
    }
    MaskProgram::new(operations, vec!["probe-mask".into()])
        .map_err(|error| format!("mask program: {error:?}"))
}
