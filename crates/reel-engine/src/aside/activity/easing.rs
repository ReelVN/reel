//! Curves are decoded once into immutable action descriptions.
use super::AdapterError;

#[derive(Clone, Copy)]
#[repr(i64)]
pub(in crate::aside) enum Easing {
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
    QuartIn,
    QuartOut,
    QuartInOut,
    ElasticIn,
    ElasticOut,
    ElasticInOut,
}

super::super::text::word_options! {
    CHOICES, easing_word -> Easing {
        b"linear" => Easing::Linear,
        b"in" => Easing::CubicIn,
        b"out" => Easing::CubicOut,
        b"in_out" => Easing::CubicInOut,
        b"quad_in" => Easing::QuadIn,
        b"quad_out" => Easing::QuadOut,
        b"quad_in_out" => Easing::QuadInOut,
        b"sine_in" => Easing::SineIn,
        b"sine_out" => Easing::SineOut,
        b"sine_in_out" => Easing::SineInOut,
        b"quint_in" => Easing::QuintIn,
        b"quint_out" => Easing::QuintOut,
        b"quint_in_out" => Easing::QuintInOut,
        b"bounce_in" => Easing::BounceIn,
        b"bounce_out" => Easing::BounceOut,
        b"bounce_in_out" => Easing::BounceInOut,
        b"quart_in" => Easing::QuartIn,
        b"quart_out" => Easing::QuartOut,
        b"quart_in_out" => Easing::QuartInOut,
        b"elastic_in" => Easing::ElasticIn,
        b"elastic_out" => Easing::ElasticOut,
        b"elastic_in_out" => Easing::ElasticInOut,
    }
}

impl Easing {
    pub(in crate::aside) fn parse(name: &str) -> Result<Self, AdapterError> {
        easing_word(name.as_bytes()).ok_or(AdapterError::Input(
            "easing requires linear or in/out/in_out with an optional quad_, quart_, quint_, sine_, bounce_ or elastic_ prefix",
        ))
    }

    pub(in crate::aside) fn from_code(code: i64) -> Self {
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
            16 => Self::QuartIn,
            17 => Self::QuartOut,
            18 => Self::QuartInOut,
            19 => Self::ElasticIn,
            20 => Self::ElasticOut,
            21 => Self::ElasticInOut,
            _ => unreachable!("private easing code"),
        }
    }

    pub(in crate::aside) fn sample(self, t: f64) -> f64 {
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
            Self::QuartIn => t.powi(4),
            Self::QuartOut => 1. - (1. - t).powi(4),
            Self::QuartInOut if t < 0.5 => 8. * t.powi(4),
            Self::QuartInOut => 1. - (-2. * t + 2.).powi(4) / 2.,
            Self::ElasticIn => 1. - elastic_out(1. - t),
            Self::ElasticOut => elastic_out(t),
            Self::ElasticInOut if t < 0.5 => (1. - elastic_out(1. - 2. * t)) / 2.,
            Self::ElasticInOut => (1. + elastic_out(2. * t - 1.)) / 2.,
        }
    }
}

fn elastic_out(t: f64) -> f64 {
    1. + 2_f64.powf(-10. * t) * ((t - 0.075) * std::f64::consts::TAU / 0.3).sin()
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
