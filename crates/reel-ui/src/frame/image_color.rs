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
