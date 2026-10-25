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
#[cfg(any(feature = "wgpu", test))]
static NORMALIZED_F16: LazyLock<[u16; 256]> =
    LazyLock::new(|| std::array::from_fn(|i| half::f16::from_f32(i as f32 / 255.).to_bits()));

#[cfg(any(feature = "gles2", test))]
static SRGB_TO_LINEAR_U8: LazyLock<[u8; 256]> =
    LazyLock::new(|| SRGB_TO_LINEAR_F16.map(linear_f16_u8));

#[cfg(any(feature = "gles2", test))]
fn linear_f16_u8(bits: u16) -> u8 {
    (half::f16::from_bits(bits).to_f32().clamp(0., 1.) * 255.).round() as u8
}

/// Convert into the GLES UNORM working texture. Opaque images use a single
/// lookup per channel; translucent pixels retain the shared color conversion.
#[cfg(any(feature = "gles2", test))]
pub(crate) fn rgba8_linear_u8(
    rgba: &[u8],
    encoding: ColorEncoding,
    alpha_type: ImageAlphaType,
) -> [u8; 4] {
    if rgba[3] == 0 {
        return [0; 4];
    }
    if rgba[3] == 255 {
        return match encoding {
            ColorEncoding::Srgb => {
                let table = &*SRGB_TO_LINEAR_U8;
                [
                    table[rgba[0] as usize],
                    table[rgba[1] as usize],
                    table[rgba[2] as usize],
                    255,
                ]
            }
            ColorEncoding::Linear => [rgba[0], rgba[1], rgba[2], 255],
        };
    }
    translucent_linear_f16(rgba, encoding, alpha_type).map(linear_f16_u8)
}

#[cfg(any(feature = "wgpu", test))]
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
    translucent_linear_f16(rgba, encoding, alpha_type)
}

fn translucent_linear_f16(
    rgba: &[u8],
    encoding: ColorEncoding,
    alpha_type: ImageAlphaType,
) -> [u16; 4] {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unorm_upload_preserves_color_and_hidden_alpha() {
        for encoding in [ColorEncoding::Srgb, ColorEncoding::Linear] {
            for alpha_type in [ImageAlphaType::Alpha, ImageAlphaType::AlphaPremultiplied] {
                for alpha in 0..=255u8 {
                    for value in 0..=255u8 {
                        let mut rgba = [value, 255 - value, value.rotate_left(3), alpha];
                        if alpha_type == ImageAlphaType::AlphaPremultiplied {
                            for channel in &mut rgba[..3] {
                                *channel = (u16::from(*channel) * u16::from(alpha) / 255) as u8;
                            }
                        }
                        assert_eq!(
                            rgba8_linear_u8(&rgba, encoding, alpha_type),
                            rgba8_linear_f16(&rgba, encoding, alpha_type).map(linear_f16_u8),
                            "encoding={encoding:?} alpha={alpha_type:?} rgba={rgba:?}",
                        );
                    }
                }
            }
        }
    }
}
