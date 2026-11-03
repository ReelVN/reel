//! Author-selected output raster density. Source and logical coordinates stay fixed.
use super::scissor::Scissor;
use reel_ui::kurbo::Affine;

pub(crate) fn size(size: [u32; 2], scale: f64) -> [u32; 2] {
    size.map(|value| (f64::from(value) * scale).ceil().max(1.) as u32)
}
pub(crate) fn transform(original: [u32; 2], reduced: [u32; 2]) -> Affine {
    Affine::scale_non_uniform(
        f64::from(reduced[0]) / f64::from(original[0]),
        f64::from(reduced[1]) / f64::from(original[1]),
    )
}
pub(crate) fn scissor(clip: Scissor, original: [u32; 2], reduced: [u32; 2]) -> Scissor {
    let scale = [0, 1].map(|i| f64::from(reduced[i]) / f64::from(original[i]));
    // Bilinear reconstruction reads neighboring texels beyond the output clip.
    let start = [clip.x, clip.y].map(f64::from);
    let end = [clip.x + clip.width, clip.y + clip.height].map(f64::from);
    let lo: [u32; 2] = std::array::from_fn(|i| (start[i] * scale[i]).floor().max(1.) as u32 - 1);
    let hi: [u32; 2] =
        std::array::from_fn(|i| ((end[i] * scale[i]).ceil() as u32 + 1).min(reduced[i]));
    Scissor {
        x: lo[0],
        y: lo[1],
        width: hi[0] - lo[0],
        height: hi[1] - lo[1],
    }
}
pub(crate) fn uv(logical: [u32; 2], storage: [u32; 2]) -> ([f32; 4], [f32; 4]) {
    let inverse = storage.map(|v| 1. / v as f32);
    (
        [
            logical[0] as f32 * inverse[0],
            logical[1] as f32 * inverse[1],
            0.,
            0.,
        ],
        [
            0.5 * inverse[0],
            0.5 * inverse[1],
            (logical[0] as f32 - 0.5) * inverse[0],
            (logical[1] as f32 - 0.5) * inverse[1],
        ],
    )
}

/// Enclose every low-grid invocation read by reconstruction in parent pixels.
pub(crate) fn demand(clip: Scissor, original: [u32; 2], scale: f64) -> Scissor {
    let reduced = size(original, scale);
    if reduced == original || clip.empty() {
        return clip;
    }
    let low = scissor(clip, original, reduced);
    let lo = [low.x, low.y];
    let hi = [low.x + low.width, low.y + low.height];
    let start: [u32; 2] = std::array::from_fn(|i| {
        (u64::from(lo[i]) * u64::from(original[i]) / u64::from(reduced[i])) as u32
    });
    let end: [u32; 2] = std::array::from_fn(|i| {
        (u64::from(hi[i]) * u64::from(original[i])).div_ceil(u64::from(reduced[i])) as u32
    });
    Scissor {
        x: start[0],
        y: start[1],
        width: end[0] - start[0],
        height: end[1] - start[1],
    }
}
