//! Exact uploaded coordinates for ordinary materialized color effects.
use reel_ui::{frame::FrameEffect, kurbo::Affine};

/// The float32 context consumed by an ordinary Vec4 fragment after storage mapping.
/// Shader identity, source content, viewport, storage, scissors and quality stay
/// in the surrounding key. Callers retain the affine fallback for crop/view
/// remapping, terminal output overrides and implicit source-clamp geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Key {
    slots: [[u32; 4]; 7],
}
impl Key {
    pub(super) fn required(
        &self,
        footprint: reel_ui::resources::EffectSourceFootprint,
        output: crate::common::scissor::Scissor,
        size: [u32; 2],
    ) -> Option<crate::common::scissor::Scissor> {
        let context = std::array::from_fn(|row| {
            self.slots
                .get(row)
                .map_or([0.; 4], |row| row.map(f32::from_bits))
        });
        crate::common::effect_plan::roi::required(&context, footprint, output, size)
    }
    #[cfg(all(feature = "profiling", any(target_os = "vita", feature = "wgpu", test)))]
    pub(super) fn changed_rows(&self, other: &Self) -> [bool; 3] {
        [
            self.slots[..2] != other.slots[..2],
            self.slots[2..4] != other.slots[2..4] || self.slots[5..] != other.slots[5..],
            self.slots[4] != other.slots[4],
        ]
    }
}

/// Describe the actual uniforms of a materialized Vec4 fullscreen invocation.
///
/// Matrix inversion, multiplication by the output size, float32 conversion and
/// storage mapping follow the draw path in that order. A rounded affine matrix
/// does not describe these operations. Scalar effects, source views, crops and
/// terminal opacity/coverage use their existing key until their actual geometry
/// and context modifications are represented together.
pub(crate) fn ordinary(
    effect: &FrameEffect,
    to_target: Affine,
    output_size: [u32; 2],
    source_size: [u32; 2],
    source_storage: [u32; 2],
) -> Option<Key> {
    // Reconstruction samples a separate raster; retain the exact affine key.
    if effect.resolution_scale != 1. {
        return None;
    }
    let mut context =
        crate::common::effect_plan::context(effect, to_target, output_size, source_size)?;
    crate::common::effect_plan::map_storage(&mut context, source_size, source_storage);
    Some(Key {
        // Rows 7/8 produce the scalar implicit-source varying and row 9 the
        // coverage varying. The ordinary Vec4 fragment consumes neither;
        // their independent rounding cannot invalidate these output pixels.
        slots: std::array::from_fn(|row| context[row].map(f32::to_bits)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::effect_result::tests::effect;

    #[test]
    fn affine_low_bits_collapse_only_after_the_actual_context_operations() {
        let effect = effect();
        let base = Affine::translate((16., 8.));
        let rounded_same = Affine::translate((16. + 2f64.powi(-45), 8.));
        assert_ne!(
            base.as_coeffs().map(f64::to_bits),
            rounded_same.as_coeffs().map(f64::to_bits),
        );
        let key = |transform| ordinary(&effect, transform, [128, 96], [64, 48], [64, 64]);
        assert_eq!(key(base), key(rounded_same));
        assert_ne!(
            key(base),
            key(Affine::translate((16. + 2f64.powi(-18), 8.)))
        );
    }

    #[test]
    fn storage_padding_and_each_consumed_row_remain_exact() {
        let mut effect = effect();
        effect.bounds = reel_ui::kurbo::Rect::new(-0.5, 0.25, 186.7, 93.25);
        let transform = Affine::new([0.8, 0.15, -0.25, 1.2, 13.75, -9.5]);
        let output = [331, 197];
        let source = [198, 119];
        let storage = [208, 128];
        let key = ordinary(&effect, transform, output, source, storage).unwrap();
        let mut uploaded =
            crate::common::effect_plan::context(&effect, transform, output, source).unwrap();
        crate::common::effect_plan::map_storage(&mut uploaded, source, storage);
        assert_eq!(
            key.slots,
            std::array::from_fn(|row| uploaded[row].map(f32::to_bits))
        );
        assert_eq!(key.slots[5][2..], storage.map(|n| (n as f32).to_bits()));
        assert_ne!(
            Some(key),
            ordinary(&effect, transform, output, source, source),
        );
        assert_ne!(
            Some(key),
            ordinary(&effect, transform, [332, 197], source, storage),
        );
        assert_ne!(
            Some(key),
            ordinary(&effect, transform, output, [197, 119], storage),
        );
    }

    #[test]
    fn unused_implicit_coordinates_do_not_invalidate_color_pixels() {
        let mut effect = effect();
        effect.bounds = reel_ui::kurbo::Rect::new(0., 0., 32., 32.);
        let a = Affine::scale(1.00000003);
        let b = Affine::scale(1.0000000301);
        let context = |transform| {
            crate::common::effect_plan::context(&effect, transform, [127, 127], [32, 32])
                .unwrap()
                .map(|row| row.map(f32::to_bits))
        };
        assert_ne!(context(a)[7..9], context(b)[7..9]);
        assert_eq!(
            ordinary(&effect, a, [127, 127], [32, 32], [32, 32]),
            ordinary(&effect, b, [127, 127], [32, 32], [32, 32]),
        );
    }

    #[test]
    fn degenerate_invocations_keep_the_existing_fallback() {
        let mut effect = effect();
        let key = |effect: &FrameEffect, transform| {
            ordinary(effect, transform, [64, 64], [32, 32], [32, 32])
        };
        assert!(key(&effect, Affine::scale(0.)).is_none());
        effect.bounds = reel_ui::kurbo::Rect::ZERO;
        assert!(key(&effect, Affine::IDENTITY).is_none());
    }
}
