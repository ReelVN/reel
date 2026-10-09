//! Analytic linear-premultiplied reference, not a rasterizer or a device test.
pub fn srgb_to_linear(value: f64) -> f64 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}
pub fn linear_premultiplied(srgb_straight: [f64; 4]) -> [f64; 4] {
    let [r, g, b, a] = srgb_straight;
    [
        srgb_to_linear(r) * a,
        srgb_to_linear(g) * a,
        srgb_to_linear(b) * a,
        a,
    ]
}
pub fn over(source: [f64; 4], destination: [f64; 4]) -> [f64; 4] {
    std::array::from_fn(|i| source[i] + destination[i] * (1. - source[3]))
}
pub fn group_result(value: [f64; 4], mask: f64, opacity: f64) -> [f64; 4] {
    value.map(|v| v * mask * opacity)
}
pub fn alpha_mask(linear_premultiplied: [f64; 4]) -> f64 {
    linear_premultiplied[3].clamp(0., 1.)
}
pub fn luminance_mask(linear_premultiplied: [f64; 4]) -> f64 {
    (linear_premultiplied[0] * 0.2126
        + linear_premultiplied[1] * 0.7152
        + linear_premultiplied[2] * 0.0722)
        .clamp(0., 1.)
}
pub fn crossfade(a: [f64; 4], b: [f64; 4], progress: f64) -> [f64; 4] {
    std::array::from_fn(|i| a[i] * (1. - progress) + b[i] * progress)
}
