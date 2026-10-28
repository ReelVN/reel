use reel_ui::frame::ImageAppearance;
pub(crate) fn appearance_uniform(appearance: ImageAppearance) -> [[f32; 4]; 10] {
    let mut values = [[0.; 4]; 10];
    // The ordinary shader path keeps its first branch and reads no other fields.
    let mode = if appearance.alpha_layout == reel_ui::frame::ImageAlphaLayout::TopBottom {
        3.
    } else {
        1.
    };
    values[4][3] = mode;
    if let Some(gradient) = appearance.gradient {
        let inverse = gradient.transform.inverse().as_coeffs();
        values[..4].copy_from_slice(&gradient.colors);
        values[4] = [
            inverse[0] as f32,
            inverse[2] as f32,
            inverse[4] as f32,
            mode,
        ];
        values[5] = [inverse[1] as f32, inverse[3] as f32, inverse[5] as f32, 1.];
    }
    let color = appearance.adjustment;
    values[6] = [
        color.gain[0],
        color.gain[1],
        color.gain[2],
        color.saturation,
    ];
    values[7] = [
        color.offset[0],
        color.offset[1],
        color.offset[2],
        f32::from(!color.is_identity()),
    ];
    values
}
