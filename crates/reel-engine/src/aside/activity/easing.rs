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
    QuintIn,
    QuintOut,
    QuintInOut,
    BounceIn,
    BounceOut,
    BounceInOut,
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
            "quint_in" => Self::QuintIn,
            "quint_out" => Self::QuintOut,
            "quint_in_out" => Self::QuintInOut,
            "bounce_in" => Self::BounceIn,
            "bounce_out" => Self::BounceOut,
            "bounce_in_out" => Self::BounceInOut,
            _ => {
                return Err(AdapterError::Input(
                    "easing requires linear, in/out/in_out, quad_in/out/in_out, sine_in/out/in_out, quint_in/out/in_out or bounce_in/out/in_out",
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
            10 => Self::QuintIn,
            11 => Self::QuintOut,
            12 => Self::QuintInOut,
            13 => Self::BounceIn,
            14 => Self::BounceOut,
            15 => Self::BounceInOut,
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
            Self::QuintIn => t.powi(5),
            Self::QuintOut => 1. - (1. - t).powi(5),
            Self::QuintInOut if t < 0.5 => 16. * t.powi(5),
            Self::QuintInOut => 1. - (-2. * t + 2.).powi(5) / 2.,
            Self::BounceIn => 1. - bounce_out(1. - t),
            Self::BounceOut => bounce_out(t),
            Self::BounceInOut if t < 0.5 => (1. - bounce_out(1. - 2. * t)) / 2.,
            Self::BounceInOut => (1. + bounce_out(2. * t - 1.)) / 2.,
        }
    }
}

fn bounce_out(t: f64) -> f64 {
    // Four parabolic arcs with successively halved durations and heights.
    let (center, floor) = if t < 4. / 11. {
        (0., 0.)
    } else if t < 8. / 11. {
        (6. / 11., 3. / 4.)
    } else if t < 10. / 11. {
        (9. / 11., 15. / 16.)
    } else {
        (21. / 22., 63. / 64.)
    };
    (121. / 16.) * (t - center).powi(2) + floor
}
