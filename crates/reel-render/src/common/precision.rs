//! Shared precision domain for bounded target composition.
//!
//! Only UNORM source/coverage multiplied by opacity in [0,1] admits binary16.
//! Each input conversion or multiplication contributes at most 2^-12 absolute
//! error (including flushed subnormal inputs). Source-over has three such
//! steps; coverage composition has five. Coordinates, windows, arbitrary
//! authored expressions, transfer functions and HDR attachments remain float32.
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
        "return textureSample(source, linear_sampler, v.source_uv) * clamp(textureSample(mask, linear_sampler, v.mask_uv).a, 0.0, 1.0) * params.opacity;",
        "return vec4<f32>(vec4<f16>(textureSample(source, linear_sampler, v.source_uv)) * f16(textureSample(mask, linear_sampler, v.mask_uv).a) * f16(params.opacity));"
    );
    std::borrow::Cow::Owned(format!("enable f16;\n{source}"))
}

#[cfg(test)]
mod tests {
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
