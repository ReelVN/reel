//! Curves are decoded once into immutable action descriptions.
use super::AdapterError;

#[derive(Clone, Copy)]
#[repr(i64)]
pub(super) enum Easing {
    Linear,
    CubicIn,
    CubicOut,
    CubicInOut,
    QuadIn,
    QuadOut,
    QuadInOut,
    SineIn,
    SineOut,
    SineInOut,
}

impl Easing {
    pub(super) fn parse(name: &str) -> Result<Self, AdapterError> {
        Ok(match name {
            "linear" => Self::Linear,
            "in" => Self::CubicIn,
            "out" => Self::CubicOut,
            "in_out" => Self::CubicInOut,
            "quad_in" => Self::QuadIn,
            "quad_out" => Self::QuadOut,
            "quad_in_out" => Self::QuadInOut,
            "sine_in" => Self::SineIn,
            "sine_out" => Self::SineOut,
            "sine_in_out" => Self::SineInOut,
            _ => {
                return Err(AdapterError::Input(
                    "easing requires linear, in, out, in_out, quad_in, quad_out, quad_in_out, sine_in, sine_out or sine_in_out",
                ));
            }
        })
    }

    pub(super) fn from_code(code: i64) -> Self {
        match code {
            0 => Self::Linear,
            1 => Self::CubicIn,
            2 => Self::CubicOut,
            3 => Self::CubicInOut,
            4 => Self::QuadIn,
            5 => Self::QuadOut,
            6 => Self::QuadInOut,
            7 => Self::SineIn,
            8 => Self::SineOut,
            9 => Self::SineInOut,
            _ => unreachable!("private easing code"),
        }
    }

    pub(super) fn sample(self, t: f64) -> f64 {
        // Trigonometric roundoff must not leave a completed tween short of its goal.
        if t == 0. || t == 1. {
            return t;
        }
        match self {
            Self::Linear => t,
            Self::CubicIn => t * t * t,
            Self::CubicOut => 1. - (1. - t).powi(3),
            Self::CubicInOut if t < 0.5 => 4. * t * t * t,
            Self::CubicInOut => 1. - (-2. * t + 2.).powi(3) / 2.,
            Self::QuadIn => t * t,
            Self::QuadOut => 1. - (1. - t).powi(2),
            Self::QuadInOut if t < 0.5 => 2. * t * t,
            Self::QuadInOut => 1. - (-2. * t + 2.).powi(2) / 2.,
            Self::SineIn => 1. - (t * std::f64::consts::FRAC_PI_2).cos(),
            Self::SineOut => (t * std::f64::consts::FRAC_PI_2).sin(),
            Self::SineInOut => 0.5 - 0.5 * (t * std::f64::consts::PI).cos(),
        }
    }
}
