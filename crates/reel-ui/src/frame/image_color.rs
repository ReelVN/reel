//! Shared image appearance values retained by widgets, frame commands and archives.
use super::*;

/// Color adjustment after uniform and bilinear tint multiplication.
/// Channels are unpremultiplied sRGB: Rec.709 saturation, then RGB gain and
/// offset, then clamp to 0..1. Alpha stays unchanged. Values must be finite;
/// saturation and gain must also be nonnegative.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImageColorAdjustment {
    /// Nonnegative Rec.709 saturation; 1 preserves the original saturation.
    pub saturation: f32,
    /// Nonnegative red, green and blue multipliers after saturation.
    pub gain: [f32; 3],
    /// Red, green and blue offsets after multiplication, before 0..1 clamping.
    pub offset: [f32; 3],
}
impl Default for ImageColorAdjustment {
    fn default() -> Self {
        Self::IDENTITY
    }
}
impl ImageColorAdjustment {
    /// No color-space conversion or adjustment is needed for this value.
    pub const IDENTITY: Self = Self {
        saturation: 1.,
        gain: [1.; 3],
        offset: [0.; 3],
    };

    /// Whether this value leaves an image unchanged.
    pub fn is_identity(self) -> bool {
        self == Self::IDENTITY
    }

    /// Whether all components satisfy the drawing contract.
    pub fn is_valid(self) -> bool {
        self.saturation.is_finite()
            && self.saturation >= 0.
            && self.gain.into_iter().all(|v| v.is_finite() && v >= 0.)
            && self.offset.into_iter().all(f32::is_finite)
    }

    /// Apply this adjustment to an already tinted, linear premultiplied color.
    /// This pure operation shares the image shader's color-space contract and
    /// leaves identity colors and transparent pixels unchanged.
    pub fn apply(self, pixel: [f32; 4]) -> [f32; 4] {
        if self.is_identity() || pixel[3] == 0. {
            return pixel;
        }
        let encode = |v: f32| {
            if v <= 0.0031308 {
                12.92 * v
            } else {
                1.055 * v.powf(1. / 2.4) - 0.055
            }
        };
        let decode = |v: f32| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let rgb = [pixel[0], pixel[1], pixel[2]].map(|v| encode((v / pixel[3]).max(0.)));
        let gray = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
        let mut result = pixel;
        for (channel, value) in rgb.into_iter().enumerate() {
            let saturated = gray + self.saturation * (value - gray);
            result[channel] =
                decode((saturated * self.gain[channel] + self.offset[channel]).clamp(0., 1.))
                    * pixel[3];
        }
        result
    }
}

/// Image-local appearance, shared by the existing cached paint and frame paths.
/// An absent appearance is the ordinary uniform-tint path.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImageAppearance {
    /// Optional bilinear corner multiplication in frame coordinates.
    pub gradient: Option<ImageColorGradient>,
    /// Adjustment after all tint multiplication.
    pub adjustment: ImageColorAdjustment,
}
impl ImageAppearance {
    /// Canonicalize uniform identity appearance to the ordinary image path.
    pub fn new(
        gradient: Option<ImageColorGradient>,
        adjustment: ImageColorAdjustment,
    ) -> Option<Self> {
        (gradient.is_some() || !adjustment.is_identity()).then_some(Self {
            gradient,
            adjustment,
        })
    }

    /// Move only the corner gradient's coordinate system; adjustment is unchanged.
    pub fn transformed(mut self, transform: Affine) -> Self {
        self.gradient = self
            .gradient
            .map(|gradient| gradient.transformed(transform));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(actual: [f32; 4], expected: [f32; 4]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!(
                (actual - expected).abs() < 0.000002,
                "{actual} != {expected}"
            );
        }
    }

    #[test]
    fn image_color_adjustment_uses_srgb_rec709_and_preserves_premultiplied_alpha() {
        let grayscale = ImageColorAdjustment {
            saturation: 0.,
            ..Default::default()
        };
        // Pure red becomes sRGB 0.2126 gray, not linear-light 0.2126.
        near(
            grayscale.apply([0.5, 0., 0., 0.5]),
            [0.01858351, 0.01858351, 0.01858351, 0.5],
        );
        let adjusted = ImageColorAdjustment {
            saturation: 0.,
            gain: [2., 0.5, 1.],
            offset: [0.1, -0.1, 0.],
        };
        // The sRGB channels after saturation, gain and offset are .5252/.0063/.2126.
        near(
            adjusted.apply([0.5, 0., 0., 0.5]),
            [0.11905582, 0.000243808, 0.01858364, 0.5],
        );
        near(
            ImageColorAdjustment {
                saturation: 1.,
                gain: [2.; 3],
                offset: [-0.25; 3],
            }
            .apply([1., 0., 0., 1.]),
            [1., 0., 0., 1.],
        );
        assert_eq!(adjusted.apply([0.; 4]), [0.; 4]);
        let original = [0.123, 0.042, 0.251, 0.7];
        assert_eq!(ImageColorAdjustment::IDENTITY.apply(original), original);
        assert!(ImageAppearance::new(None, ImageColorAdjustment::IDENTITY).is_none());
    }
}
