use reel_ui::frame::ImageAppearance;
/// Signed view extents retain repeat axes without another material uniform slot.
/// The complete physical texture needs neither a view clamp nor custom wrapping.
pub(crate) fn view_bounds(
    layout: reel_core::texture::TextureLayout,
    origin: [u32; 2],
    repeat: [bool; 2],
) -> [f32; 4] {
    if origin == [0; 2] && layout.content_size == layout.storage_size {
        return [0.; 4];
    }
    [
        origin[0] as f32 / layout.storage_size[0] as f32,
        origin[1] as f32 / layout.storage_size[1] as f32,
        layout.content_size[0] as f32 / layout.storage_size[0] as f32
            * if repeat[0] { -1. } else { 1. },
        layout.content_size[1] as f32 / layout.storage_size[1] as f32
            * if repeat[1] { -1. } else { 1. },
    ]
}
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

/// Block-alpha sources and premultiplied vertex colors preserve zero RGB at
/// zero alpha. A constant RGB adjustment can then use only the sampled alpha.
#[cfg(any(feature = "gxm", test))]
pub(crate) fn constant_alpha_tint(
    sampling: Option<super::mesh::DirectPaint>,
    appearance: &[[f32; 4]; 10],
    mut colors: impl Iterator<Item = [f32; 4]>,
) -> bool {
    use super::mesh::DirectPaint;
    matches!(
        sampling,
        Some(
            DirectPaint::Bc1
                | DirectPaint::Bc3
                | DirectPaint::RegionalBc1
                | DirectPaint::RegionalBc3
        )
    ) && matches!(appearance[4][3], 0. | 1.)
        && appearance[5][3] == 0.
        && appearance[7][3] != 0.
        && appearance[6][..3] == [0.; 3]
        && appearance[7][..3].iter().all(|v| v.is_finite())
        && colors.all(|color| {
            (0.0..=1.0).contains(&color[3])
                && color[..3].iter().all(|v| (0.0..=color[3]).contains(v))
        })
}

/// A zero-saturation adjustment with equal channel gains/offsets has one
/// scalar result. Opaque block RGB with white premultiplied vertex colors
/// also lets opacity move after the transfer functions. Restrict that
/// cancellation to |gain| <= 2 so its f32 rounding stays below one UNORM byte.
#[cfg(any(feature = "gxm", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Monochrome {
    Premultiplied,
    OpaqueWhite,
}

#[cfg(any(feature = "gxm", test))]
pub(crate) fn monochrome_adjustment(
    sampling: Option<super::mesh::DirectPaint>,
    appearance: &[[f32; 4]; 10],
    colors: impl Iterator<Item = [f32; 4]>,
) -> Option<Monochrome> {
    use super::mesh::DirectPaint;
    if !matches!(
        sampling,
        Some(
            DirectPaint::Bc1
                | DirectPaint::Bc3
                | DirectPaint::RegionalBc1
                | DirectPaint::RegionalBc3
        )
    ) || !matches!(appearance[4][3], 0. | 1.)
        || appearance[5][3] != 0.
        || appearance[7][3] == 0.
        || appearance[6][3] != 0.
        || ![6, 7].into_iter().all(|row| {
            appearance[row][0].is_finite()
                && appearance[row][..3]
                    .iter()
                    .all(|v| *v == appearance[row][0])
        })
    {
        return None;
    }
    let mut opaque_white = matches!(sampling, Some(DirectPaint::Bc1 | DirectPaint::RegionalBc1))
        && appearance[6][0].abs() <= 2.;
    for color in colors {
        if !(0.0..=1.0).contains(&color[3])
            || !color[..3].iter().all(|v| (0.0..=color[3]).contains(v))
        {
            return None;
        }
        opaque_white &= color[..3].iter().all(|v| *v == color[3]);
    }
    Some(if opaque_white {
        Monochrome::OpaqueWhite
    } else {
        Monochrome::Premultiplied
    })
}

#[cfg(test)]
#[path = "appearance_tests.rs"]
mod tests;
