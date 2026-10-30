//! Shared precision domain for bounded target composition.
//!
//! Only UNORM source/coverage multiplied by opacity in [0,1] admits binary16.
//! Each input conversion or multiplication contributes at most 2^-12 absolute
//! error (including flushed subnormal inputs). Source-over has three such
//! steps; coverage composition has five. Coordinates, windows, arbitrary
//! authored expressions, transfer functions and HDR attachments remain float32.
//! GXM transitions also admit bounded UNORM interpolation. Including input,
//! progress, difference, product and sum rounding gives <=7*2^-12 absolute error.
/// Construction-time proof that all vertex color components lie in [0, 1].
/// Unknown, negative, HDR and non-finite inputs retain float32 paint arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BoundedColor(bool);
impl BoundedColor {
    pub const UNKNOWN: Self = Self(false);
    pub fn new(color: [f32; 4]) -> Self {
        Self(color.into_iter().all(|v| (0.0..=1.0).contains(&v)))
    }
    pub fn intersect(self, other: Self) -> Self {
        Self(self.0 && other.0)
    }
    pub fn paint(
        self,
        direct: Option<super::mesh::DirectPaint>,
        format: Option<reel_core::texture::TextureFormat>,
    ) -> bool {
        use super::mesh::DirectPaint;
        use reel_core::texture::TextureFormat;
        self.0
            && match direct {
                Some(DirectPaint::Solid) => true,
                Some(DirectPaint::Rgba | DirectPaint::Bc1 | DirectPaint::Bc3) => matches!(
                    format,
                    Some(
                        TextureFormat::Rgba8
                            | TextureFormat::Bgra8
                            | TextureFormat::Bc1Rgb
                            | TextureFormat::Bc3Rgba
                    )
                ),
                _ => false,
            }
    }
}

#[cfg(feature = "wgpu")]
pub(crate) fn composite_wgsl(
    source: &str,
    unorm: bool,
    native_f16: bool,
) -> std::borrow::Cow<'_, str> {
    if !unorm || !native_f16 {
        return std::borrow::Cow::Borrowed(source);
    }
    let source = source.replace(
        "return textureSample(source, linear_sampler, v.source_uv) * params.opacity;",
        "return vec4<f32>(vec4<f16>(textureSample(source, linear_sampler, v.source_uv)) * f16(params.opacity));"
    ).replace(
        "return textureSample(source, linear_sampler, v.source_uv) * clamp(mask_alpha(v), 0.0, 1.0) * params.opacity;",
        "return vec4<f32>(vec4<f16>(textureSample(source, linear_sampler, v.source_uv)) * f16(mask_alpha(v)) * f16(params.opacity));"
    );
    std::borrow::Cow::Owned(format!("enable f16;\n{source}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn paint_proof_rejects_hdr_unknown_and_non_finite_inputs() {
        use super::BoundedColor;
        use crate::common::mesh::DirectPaint;
        use reel_core::texture::TextureFormat;
        for value in [-0.01, 1.01, f32::NAN, f32::INFINITY] {
            assert!(!BoundedColor::new([value; 4]).paint(Some(DirectPaint::Solid), None));
        }
        let bounded = BoundedColor::new([0., 0.25, 0.5, 1.]);
        assert!(bounded.paint(Some(DirectPaint::Solid), None));
        assert!(bounded.paint(Some(DirectPaint::Rgba), Some(TextureFormat::Rgba8)));
        assert!(!bounded.paint(
            Some(DirectPaint::Rgba),
            Some(TextureFormat::LinearRgba16Float)
        ));
        assert!(!bounded.paint(Some(DirectPaint::Rgba), None));
        assert!(!bounded.paint(Some(DirectPaint::RegionalRgba), Some(TextureFormat::Rgba8)));
        assert!(
            !bounded
                .intersect(BoundedColor::UNKNOWN)
                .paint(Some(DirectPaint::Solid), None)
        );
    }
    #[test]
    fn bounded_paint_interpolation_and_product_error() {
        // Include flush-to-zero hardware, endpoints and values near f16 boundaries.
        let h = |v: f32| {
            let v = half(v);
            if v.abs() < 2f32.powi(-14) { 0. } else { v }
        };
        let colors = [
            0.,
            1.,
            0.1,
            0.51,
            1. / 255.,
            0.99999,
            2f32.powi(-14),
            2f32.powi(-15),
        ];
        let mut maximum = [0f32; 2];
        for a in colors {
            for b in colors {
                for weight in 0..=64 {
                    let t = weight as f32 / 64.;
                    let expected = a * (1. - t) + b * t;
                    let actual = h(h(a) * (1. - t) + h(b) * t);
                    maximum[0] = maximum[0].max((actual - expected).abs());
                    for source in 0..=64 {
                        let source = source as f32 / 64.;
                        maximum[1] =
                            maximum[1].max((h(h(source) * actual) - source * expected).abs());
                    }
                }
            }
        }
        assert!(maximum[0] <= 2. / 4096.);
        assert!(maximum[1] <= 4. / 4096.);
        // Independent color and alpha errors contribute at most 8/4096 in source-over.
        eprintln!("bounded paint interpolation/product maximum absolute error: {maximum:?}");
    }
    #[cfg(feature = "wgpu")]
    #[test]
    fn bounded_paint_shader_validates_with_float_positions_and_uvs() {
        let source = include_str!("../gpu/paint_bounded.wgsl");
        let module = wgpu::naga::front::wgsl::parse_str(source).unwrap();
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
        assert!(source.contains("position: vec4<f32>"));
        assert!(source.contains("uv: vec2<f32>"));
    }
    fn half(value: f32) -> f32 {
        half::f16::from_f32(value).to_f32()
    }
    #[test]
    fn bounded_products_and_translucent_backdrops_respect_the_shared_error_domain() {
        let mut largest = [0f32; 2];
        let mut blend_error = 0f32;
        for source in 0..=255 {
            for mask in 0..=255 {
                for opacity in [0., 0.00001, 0.001, 0.13, 0.51, 0.9, 1.] {
                    let source = source as f32 / 255.;
                    let mask = mask as f32 / 255.;
                    let ordinary = source * opacity;
                    let compact = half(half(source) * half(opacity));
                    largest[0] = largest[0].max((ordinary - compact).abs());
                    let ordinary = ordinary * mask;
                    let compact = half(half(half(source) * half(mask)) * half(opacity));
                    largest[1] = largest[1].max((ordinary - compact).abs());
                    assert!((ordinary - compact).abs() <= 5. / 4096.);
                    let alpha = source + (1. - source) * 0.43;
                    let ordinary_alpha = alpha * mask * opacity;
                    let compact_alpha = half(half(half(alpha) * half(mask)) * half(opacity));
                    for background in [0., 0.37, 1.] {
                        let expected = ordinary + background * (1. - ordinary_alpha);
                        let actual = compact + background * (1. - compact_alpha);
                        blend_error = blend_error.max((actual - expected).abs());
                    }
                }
            }
        }
        assert!(largest[0] <= 3. / 4096.);
        // Source color and alpha errors can have opposite signs at blend time.
        // Five rounding steps per input imply <=10/4096 RGB error, below 1/255.
        assert!(blend_error <= 10. / 4096.);
        assert!(blend_error < 1. / 255.);
        eprintln!(
            "bounded binary16 multiply maximum errors: {largest:?}; source-over RGB={blend_error}"
        );
    }
    #[test]
    fn bounded_transition_rounding_stays_within_one_unorm_byte() {
        let h = |value: f32| {
            let rounded = half(value);
            if rounded.abs() < 2f32.powi(-14) {
                0.
            } else {
                rounded
            }
        };
        let mut inputs = (0..=255).map(|v| v as f32 / 255.).collect::<Vec<_>>();
        // Filtered samples can lie between encoded bytes or near half boundaries.
        inputs.extend([0.00001, 0.49988, 0.50012, 0.50025, 0.99976, 0.99999]);
        let mut maximum = 0f64;
        let byte = |v: f64| (v.clamp(0., 1.) * 255.).round() as u8;
        for &source in &inputs {
            for &target in &inputs {
                for progress in [
                    0.,
                    0.00001,
                    0.0001,
                    1. / 255.,
                    0.1,
                    0.13,
                    0.5,
                    0.51,
                    0.9,
                    0.999,
                    1.,
                ] {
                    // Reference uses double precision and the convex-combination
                    // form, independently of the shader's difference/FMA form.
                    let expected = f64::from(source) * (1. - f64::from(progress))
                        + f64::from(target) * f64::from(progress);
                    let difference = h(h(target) - h(source));
                    let actual = f64::from(h(h(difference * h(progress)) + h(source)));
                    maximum = maximum.max((actual - expected).abs());
                    assert!(byte(actual).abs_diff(byte(expected)) <= 1);
                }
            }
        }
        assert!(maximum <= 7. / 4096.);
        assert!(maximum < 1. / 255.);
        eprintln!("bounded transition maximum absolute error: {maximum}");
    }
    #[cfg(feature = "wgpu")]
    #[test]
    fn wgsl_precision_selection_retains_float_coordinates_and_hdr_fallback() {
        use wgpu::naga;
        let source = include_str!("../gpu/composite.wgsl");
        for (unorm, f16) in [(false, false), (false, true), (true, false), (true, true)] {
            let generated = super::composite_wgsl(source, unorm, f16);
            assert_eq!(generated.starts_with("enable f16;"), unorm && f16);
            assert!(generated.contains("v.source_uv"));
            let module = naga::front::wgsl::parse_str(&generated).unwrap();
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap();
        }
    }
}
