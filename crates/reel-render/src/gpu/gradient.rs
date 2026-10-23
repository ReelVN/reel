//! Two-stop linear paint shares the image pipeline and glyph coverage atlas.
use super::GpuError;
use reel_ui::{
    frame::RecordLocation,
    kurbo::Affine,
    peniko::{Extend, Gradient, GradientKind, InterpolationAlphaSpace, color::ColorSpaceTag},
};

pub(super) type Uniform = [[f32; 4]; 8];

/// Map window pixel centers into the brush's declared interpolation space.
/// Glyph outline transforms belong to coverage rasterization, not this mapping.
pub(super) fn uniform(
    gradient: &Gradient,
    transform: Affine,
    at: RecordLocation,
) -> Result<Option<Uniform>, GpuError> {
    let GradientKind::Linear(line) = gradient.kind else {
        return Err(GpuError::Unsupported {
            at,
            record: "non-linear gradient",
        });
    };
    let [from, to] = gradient.stops.as_slice() else {
        return Err(GpuError::Unsupported {
            at,
            record: "gradient requires two stops",
        });
    };
    let srgb = match gradient.interpolation_cs {
        ColorSpaceTag::Srgb => true,
        ColorSpaceTag::LinearSrgb => false,
        _ => {
            return Err(GpuError::Unsupported {
                at,
                record: "gradient interpolation color space",
            });
        }
    };
    if transform.determinant() == 0. {
        return Ok(None);
    }
    let premultiplied =
        gradient.interpolation_alpha_space == InterpolationAlphaSpace::Premultiplied;
    let color = |stop: &reel_ui::peniko::ColorStop| {
        let mut c = stop.color.convert(gradient.interpolation_cs).components;
        if premultiplied {
            let alpha = c[3];
            for channel in &mut c[..3] {
                *channel *= alpha;
            }
        }
        c
    };
    let mut values = [[0.; 4]; 8];
    values[0] = color(from);
    values[1] = color(to);
    let direction = line.end - line.start;
    let length_squared = direction.hypot2();
    if length_squared == 0. {
        values[1] = values[0];
        values[4] = [0., 0., 0., 2.];
    } else {
        let axis = direction / length_squared;
        let inverse = transform.inverse().as_coeffs();
        values[4] = [
            (axis.x * inverse[0] + axis.y * inverse[1]) as f32,
            (axis.x * inverse[2] + axis.y * inverse[3]) as f32,
            (axis.x * (inverse[4] - line.start.x) + axis.y * (inverse[5] - line.start.y)) as f32,
            2.,
        ];
    }
    values[5] = [
        match gradient.extend {
            Extend::Pad => 0.,
            Extend::Repeat => 1.,
            Extend::Reflect => 2.,
        },
        f32::from(srgb),
        f32::from(premultiplied),
        0.,
    ];
    values[6] = [from.offset, to.offset, 0., 0.];
    if from.offset > to.offset || values.iter().flatten().any(|v| !v.is_finite()) {
        return Err(GpuError::Unsupported {
            at,
            record: "invalid linear gradient parameters",
        });
    }
    Ok(Some(values))
}
