use super::GpuError;
pub fn upload_cost(size: [u32; 2]) -> Result<u64, GpuError> {
    // Convert directly into mapped staging. Only staging and GPU residency coexist.
    let row = u64::from(size[0])
        .checked_mul(8)
        .ok_or(GpuError::InvalidSize)?;
    let aligned = row.div_ceil(256) * 256;
    (aligned + row)
        .checked_mul(u64::from(size[1]))
        .ok_or(GpuError::InvalidSize)
}

use reel_ui::{
    peniko::{
        ImageAlphaType,
        color::{AlphaColor, LinearSrgb, PremulColor, Srgb},
    },
    resources::ColorEncoding,
};
use std::sync::LazyLock;
static SRGB_TO_LINEAR: LazyLock<[f32; 256]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        AlphaColor::<Srgb>::new([i as f32 / 255., 0., 0., 1.])
            .convert::<LinearSrgb>()
            .components[0]
    })
});

static SRGB_TO_LINEAR_F16: LazyLock<[u16; 256]> =
    LazyLock::new(|| std::array::from_fn(|i| half::f16::from_f32(SRGB_TO_LINEAR[i]).to_bits()));
static NORMALIZED_F16: LazyLock<[u16; 256]> =
    LazyLock::new(|| std::array::from_fn(|i| half::f16::from_f32(i as f32 / 255.).to_bits()));

pub(crate) fn rgba8_linear_f16(
    rgba: &[u8],
    encoding: ColorEncoding,
    alpha_type: ImageAlphaType,
) -> [u16; 4] {
    if rgba[3] == 0 {
        // Hidden RGB must not become visible in interpolation or masks.
        return [0; 4];
    }
    if rgba[3] == 255 {
        // Opaque channels have only 256 possible results. Reuse their exact
        // half encodings rather than converting every channel of every pixel.
        let table = match encoding {
            ColorEncoding::Srgb => &*SRGB_TO_LINEAR_F16,
            ColorEncoding::Linear => &*NORMALIZED_F16,
        };
        return [
            table[rgba[0] as usize],
            table[rgba[1] as usize],
            table[rgba[2] as usize],
            0x3c00,
        ];
    }
    let components = std::array::from_fn(|i| f32::from(rgba[i]) / 255.);
    let linear = match (encoding, alpha_type) {
        (ColorEncoding::Srgb, ImageAlphaType::Alpha) => {
            let alpha = components[3];
            [
                SRGB_TO_LINEAR[rgba[0] as usize] * alpha,
                SRGB_TO_LINEAR[rgba[1] as usize] * alpha,
                SRGB_TO_LINEAR[rgba[2] as usize] * alpha,
                alpha,
            ]
        }
        (ColorEncoding::Srgb, ImageAlphaType::AlphaPremultiplied) => {
            PremulColor::<Srgb>::new(components)
                .convert::<LinearSrgb>()
                .components
        }
        (ColorEncoding::Linear, ImageAlphaType::Alpha) => {
            AlphaColor::<LinearSrgb>::new(components)
                .premultiply()
                .components
        }
        (ColorEncoding::Linear, ImageAlphaType::AlphaPremultiplied) => components,
    };
    linear.map(|value| half::f16::from_f32(value).to_bits())
}
