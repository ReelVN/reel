//! Shared parameter, coordinate and resource admission for prepared effects.
use super::GpuError;
use reel_ui::{
    frame::{FrameDrawing, FrameEffect, RecordLocation},
    kurbo::Affine,
    resources::{EffectShaderDescriptor, ResourceData, ResourceLease},
    stage::effect::EffectValue,
};

pub(crate) struct Shader<'a> {
    pub lease: &'a ResourceLease,
    pub descriptor: &'a EffectShaderDescriptor,
    pub bytes: &'a [u8],
}
/// Complete local source domain, independent of an ancestor's output clipping.
pub(crate) struct SourceLayout {
    pub size: [u32; 2],
    pub frame_to_source: Affine,
}

/// A sole effect child can transfer its result to the isolated parent. Exact
/// grid equality preserves sample positions and avoids an intermediate copy;
/// other composites retain their independent source and destination.
pub(crate) fn forwards_source(
    group: &reel_ui::frame::FrameGroup,
    source: &SourceLayout,
    parent_transform: Affine,
    parent_size: [u32; 2],
) -> bool {
    source.size == parent_size
        && source.frame_to_source == parent_transform
        && group.effect.is_some()
        && group.mask_effect.is_none()
        && group.composite.alpha == 1.
        && group.composite.blend == Default::default()
        && group.invert == 0.
        && group.clip.is_none()
        && group.mask.is_none()
        && group.image_mask.is_none()
        && group.filters.is_empty()
}

/// Keep complete local effect results through dependencies that consume a
/// projected source. A plain final source-over can project into its parent.
pub(crate) fn direct_mask_projection(
    group: &reel_ui::frame::FrameGroup,
    retained_transitions: bool,
) -> bool {
    group.mask_effect.is_some()
        && !retained_transitions
        && group.composite.blend == Default::default()
        && group.invert == 0.
        && group.clip.is_none()
        && group.mask.is_none()
        && group.image_mask.is_none()
        && group.filters.is_empty()
}
pub(crate) fn source_layout(
    effect: &FrameEffect,
    effect_to_frame: Affine,
    outer: Affine,
    max_dimension: u32,
    at: RecordLocation,
) -> Result<SourceLayout, GpuError> {
    let physical = outer * effect_to_frame;
    let [a, b, c, d, _, _] = physical.as_coeffs();
    let bounds = effect.bounds;
    let extent = [
        bounds.width().max(0.) * a.hypot(b),
        bounds.height().max(0.) * c.hypot(d),
    ];
    if extent
        .iter()
        .any(|v| !v.is_finite() || v.ceil() > f64::from(max_dimension))
    {
        return Err(GpuError::Unsupported {
            at,
            record: "effect source exceeds the device texture dimensions",
        });
    }
    let size = extent.map(|v| v.ceil().max(1.) as u32);
    let frame_to_source =
        if bounds.width() > 0. && bounds.height() > 0. && effect_to_frame.determinant() != 0. {
            Affine::scale_non_uniform(
                f64::from(size[0]) / bounds.width(),
                f64::from(size[1]) / bounds.height(),
            ) * Affine::translate((-bounds.x0, -bounds.y0))
                * effect_to_frame.inverse()
        } else {
            Affine::scale(0.)
        };
    Ok(SourceLayout {
        size,
        frame_to_source,
    })
}
pub(crate) fn shader<'a>(
    drawing: &'a FrameDrawing,
    effect: &FrameEffect,
    at: RecordLocation,
) -> Result<Shader<'a>, GpuError> {
    let lease = drawing
        .resources()
        .leases()
        .iter()
        .find(|lease| lease.handle() == effect.shader)
        .ok_or(GpuError::InvalidRecord {
            at,
            record: "effect shader resource",
        })?;
    let ResourceData::EffectShader { descriptor, bytes } = lease.data() else {
        return Err(GpuError::InvalidRecord {
            at,
            record: "effect shader resource kind",
        });
    };
    // FrameBuild and retained restoration validate the descriptor and complete
    // value schema before publishing the immutable drawing.
    Ok(Shader {
        lease,
        descriptor,
        bytes: bytes.data(),
    })
}

pub(crate) fn context(
    effect: &FrameEffect,
    to_target: Affine,
    size: [u32; 2],
    source_size: [u32; 2],
) -> Option<[[f32; 4]; crate::effect::CONTEXT_SLOTS]> {
    let bounds = effect.bounds;
    if bounds.width() <= 0. || bounds.height() <= 0. || to_target.determinant() == 0. {
        return None;
    }
    let uv_to_target = to_target
        * Affine::translate((bounds.x0, bounds.y0))
        * Affine::scale_non_uniform(bounds.width(), bounds.height());
    let target_to_uv = uv_to_target.inverse();
    let rows = |a: Affine| {
        let [a, b, c, d, e, f] = a.as_coeffs();
        [
            [a as f32, c as f32, e as f32, 0.],
            [b as f32, d as f32, f as f32, 0.],
        ]
    };
    let [a, b] = rows(target_to_uv);
    let [implicit_a, implicit_b] =
        rows(target_to_uv * Affine::scale_non_uniform(f64::from(size[0]), f64::from(size[1])));
    let slots = [
        a,
        b,
        [1., 0., 0., 0.],
        [0., 1., 0., 0.],
        [
            bounds.x0 as f32,
            bounds.y0 as f32,
            bounds.width() as f32,
            bounds.height() as f32,
        ],
        [
            size[0] as f32,
            size[1] as f32,
            source_size[0] as f32,
            source_size[1] as f32,
        ],
        [0., 0., 1., 1.],
        implicit_a,
        implicit_b,
    ];
    slots
        .iter()
        .flatten()
        .all(|v| v.is_finite())
        .then_some(slots)
}

/// Explicitly map parent frame pixels to the supplied source storage. Coverage
/// consumers pass this view instead of inferring the source domain from its size.
pub(crate) fn map_source(
    context: &mut [[f32; 4]; crate::effect::CONTEXT_SLOTS],
    effect: &FrameEffect,
    to_target: Affine,
    frame_to_source: Affine,
    source_size: [u32; 2],
) {
    let bounds = effect.bounds;
    let uv_to_frame = to_target
        * Affine::translate((bounds.x0, bounds.y0))
        * Affine::scale_non_uniform(bounds.width(), bounds.height());
    let uv_to_source = Affine::scale_non_uniform(
        1.0 / f64::from(source_size[0]),
        1.0 / f64::from(source_size[1]),
    ) * frame_to_source
        * uv_to_frame;
    let [a, b, c, d, e, f] = uv_to_source.as_coeffs();
    context[2] = [a as f32, c as f32, e as f32, 0.0];
    context[3] = [b as f32, d as f32, f as f32, 0.0];
    // The implicit current-pixel sample follows the actual source view directly,
    // without the local UV inverse/forward round trip used by authored samples.
    let implicit = Affine::scale_non_uniform(
        1.0 / f64::from(source_size[0]),
        1.0 / f64::from(source_size[1]),
    ) * frame_to_source
        * Affine::scale_non_uniform(f64::from(context[5][0]), f64::from(context[5][1]));
    let [a, b, c, d, e, f] = implicit.as_coeffs();
    context[7] = [a as f32, c as f32, e as f32, 0.0];
    context[8] = [b as f32, d as f32, f as f32, 0.0];
}
/// Common fixed image metadata: content scale/encoding/format plus texel centers.
pub(crate) fn texture_metadata(
    layout: reel_core::texture::TextureLayout,
    encoding: reel_ui::resources::ColorEncoding,
    format: Option<reel_core::texture::TextureFormat>,
) -> [[f32; 4]; 2] {
    use reel_core::texture::TextureFormat;
    [
        [
            layout.content_size[0] as f32 / layout.storage_size[0] as f32,
            layout.content_size[1] as f32 / layout.storage_size[1] as f32,
            f32::from(encoding == reel_ui::resources::ColorEncoding::Linear),
            match format {
                Some(TextureFormat::Bc1Rgb) => 1.0,
                Some(TextureFormat::Bc3Rgba) => 2.0,
                _ => 0.0,
            },
        ],
        [
            0.5 / layout.storage_size[0] as f32,
            0.5 / layout.storage_size[1] as f32,
            0.0,
            0.0,
        ],
    ]
}

pub(crate) fn slot(value: EffectValue) -> [f32; 4] {
    match value {
        EffectValue::Bool(v) => [u8::from(v) as f32, 0., 0., 0.],
        EffectValue::Float(v) => [f32::from_bits(v), 0., 0., 0.],
        EffectValue::Vec2(v) => [f32::from_bits(v[0]), f32::from_bits(v[1]), 0., 0.],
        EffectValue::Vec3(v) => [
            f32::from_bits(v[0]),
            f32::from_bits(v[1]),
            f32::from_bits(v[2]),
            0.,
        ],
        EffectValue::Vec4(v) => v.map(f32::from_bits),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_ui::core::Widget;
    #[test]
    fn transformed_local_coordinates_and_source_pixels_share_one_mapping() {
        let effect = FrameEffect {
            definition: [0; 32],
            shader: reel_ui::resources::ResourceHandle {
                registry: 1,
                slot: 0,
                generation: 1,
            },
            values: vec![],
            images: vec![],
            bounds: reel_ui::kurbo::Rect::new(4., 8., 104., 58.),
            coverage_invert: false,
        };
        let transform = Affine::translate((30., 20.))
            * Affine::rotate(0.3)
            * Affine::scale_non_uniform(2., 0.75);
        let at = RecordLocation {
            fragment: 0,
            command: 0,
            mask_depth: 0,
            owner: reel_ui::widgets::Label::new("").prepare().id(),
        };
        let layout = source_layout(&effect, transform, Affine::IDENTITY, 4096, at).unwrap();
        assert_eq!(layout.size, [200, 38]);
        let context = context(&effect, transform, [960, 544], layout.size).unwrap();
        let uv = [0.25f32, 0.7];
        let local = reel_ui::kurbo::Point::new(
            effect.bounds.x0 + f64::from(uv[0]) * effect.bounds.width(),
            effect.bounds.y0 + f64::from(uv[1]) * effect.bounds.height(),
        );
        let target = transform * local;
        let row = |index: usize, x: f32, y: f32| {
            context[index][0] * x + context[index][1] * y + context[index][2]
        };
        assert!((row(0, target.x as f32, target.y as f32) - uv[0]).abs() < 1e-5);
        assert!((row(1, target.x as f32, target.y as f32) - uv[1]).abs() < 1e-5);
        let source = layout.frame_to_source * target;
        assert!((row(2, uv[0], uv[1]) * layout.size[0] as f32 - source.x as f32).abs() < 1e-4);
        assert!((row(3, uv[0], uv[1]) * layout.size[1] as f32 - source.y as f32).abs() < 1e-4);
        assert!((row(7, target.x as f32 / 960., target.y as f32 / 544.) - uv[0]).abs() < 1e-5);
        assert!((row(8, target.x as f32 / 960., target.y as f32 / 544.) - uv[1]).abs() < 1e-5);
        // Output viewport/translation cannot truncate or move the source domain.
        let offscreen = source_layout(
            &effect,
            Affine::translate((-1000., -700.)) * transform,
            Affine::IDENTITY,
            4096,
            at,
        )
        .unwrap();
        assert_eq!(offscreen.size, layout.size);
        assert_eq!(context[5], [960., 544., 200., 38.]);
        assert_eq!(context[6], [0., 0., 1., 1.]);
        // The wrapper clamps to these local texel centers, not the output FBO.
        let first = [0.5 / context[5][2], 0.5 / context[5][3]];
        let last = [1. - first[0], 1. - first[1]];
        assert_eq!(first[0] * layout.size[0] as f32, 0.5);
        assert_eq!(last[0] * layout.size[0] as f32, 199.5);
    }

    #[test]
    fn implicit_source_uses_the_local_pixel_grid_and_preserves_padded_content_centers() {
        let effect = FrameEffect {
            definition: [0; 32],
            shader: reel_ui::resources::ResourceHandle {
                registry: 1,
                slot: 0,
                generation: 1,
            },
            values: vec![],
            images: vec![],
            bounds: reel_ui::kurbo::Rect::new(4., 8., 104., 58.),
            coverage_invert: false,
        };
        let target_size = [200, 38];
        for transform in [
            Affine::IDENTITY,
            Affine::translate((-30., 55.))
                * Affine::rotate(0.3)
                * Affine::scale_non_uniform(2., 0.75),
            Affine::scale_non_uniform(-0.25, 3.),
        ] {
            for (source_size, view, origin) in [
                ([200, 38], Affine::IDENTITY, [0., 0.]),
                ([256, 64], Affine::translate((8., 12.)), [8., 12.]),
            ] {
                let mut context = context(&effect, transform, target_size, source_size).unwrap();
                map_source(&mut context, &effect, transform, view, source_size);
                let mut largest_error = 0_f32;
                for y in 0..target_size[1] {
                    for x in 0..target_size[0] {
                        let target = [x as f32 + 0.5, y as f32 + 0.5];
                        let normalized = [
                            target[0] / target_size[0] as f32,
                            target[1] / target_size[1] as f32,
                        ];
                        for axis in 0..2 {
                            let row = context[7 + axis];
                            let sampled = row[0] * normalized[0] + row[1] * normalized[1] + row[2];
                            let expected = (target[axis] + origin[axis]) / source_size[axis] as f32;
                            assert!(
                                (sampled - expected).abs() * (source_size[axis] as f32) < 0.0001
                            );
                            if source_size == target_size {
                                assert_eq!(
                                    sampled.to_bits(),
                                    normalized[axis].to_bits(),
                                    "local source retains the original normalized sample grid"
                                );
                            }
                            // SGX interpolation has a 15-bit mantissa. The
                            // implicit local path uses the same varying grid as
                            // ordinary source composition; record its error.
                            let iterated = f32::from_bits(sampled.to_bits() & !0xff);
                            largest_error = largest_error
                                .max((iterated - expected).abs() * source_size[axis] as f32);
                            let low = (origin[axis] + 0.5) / source_size[axis] as f32;
                            let high = (origin[axis] + target_size[axis] as f32 - 0.5)
                                / source_size[axis] as f32;
                            assert_eq!(
                                expected.clamp(low, high).to_bits(),
                                expected.to_bits(),
                                "padding does not enter the local source domain"
                            );
                        }
                    }
                }
                assert!(largest_error <= 1. / 128.);
            }
        }
    }
}
