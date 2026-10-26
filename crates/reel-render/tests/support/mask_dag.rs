// Shared by the scalar-storage oracle, shader validation and native submission.
pub fn operations() -> Vec<reel_ui::stage::MaskOp> {
    use reel_ui::stage::{
        MaskChannel::{Alpha, Gray},
        MaskOp::*,
    };
    let sample = |image, channel, coordinates, repeat| Sample {
        image,
        channel,
        coordinates,
        repeat,
    };
    vec![
        Constant(0_f32.to_bits()),
        Constant(1_f32.to_bits()),
        Parameter(0),
        sample(0, Alpha, [0, 0, 1, 1], 0),
        sample(1, Gray, [2, 0, 1, 1], 1),
        Subtract(3, 4),
        Multiply(5, 2),
        Clamp(6, 0_f32.to_bits(), 1_f32.to_bits()),
        Constant(2_f32.to_bits()),
        Pow(7, 2_f32.to_bits()),
        Constant((-1_f32).to_bits()),
        Add(9, 10),
        Smoothstep(10, 1, 11),
        sample(2, Gray, [7, 2, 1, 1], 2),
        Subtract(12, 13),
        Clamp(14, (-2_f32).to_bits(), 2_f32.to_bits()),
        sample(3, Alpha, [15, 14, 2, 1], 3),
        Pow(16, 0_f32.to_bits()),
        Add(15, 17),
        Multiply(18, 8),
        Clamp(19, (-1_f32).to_bits(), 3_f32.to_bits()),
        Smoothstep(0, 0, 20),
        Pow(20, 0.5_f32.to_bits()),
        Subtract(22, 17),
        Multiply(23, 13),
        Add(24, 21),
        sample(3, Gray, [23, 25, 2, 9], 3),
        Smoothstep(17, 10, 26),
        Subtract(27, 26),
        Add(28, 18),
        Multiply(29, 7),
        Clamp(30, 0_f32.to_bits(), 1_f32.to_bits()),
    ]
}

pub fn compact_operations() -> Vec<reel_ui::stage::MaskOp> {
    let mut result = operations();
    result.truncate(16);
    while result.len() < 32 {
        result.push(reel_ui::stage::MaskOp::Pow(
            result.len() as u32 - 1,
            2_f32.to_bits(),
        ));
    }
    result
}
