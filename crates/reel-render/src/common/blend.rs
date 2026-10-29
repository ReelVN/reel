//! Shared composite shader operation codes. Keep both shader dispatches in sync.
use reel_ui::frame::BlendMode;
use reel_ui::peniko::{Compose, Mix};

/// How an already isolated source obtains the destination for its final blend.
/// The current-pixel path requires quantized RGBA8 inputs and a single sample;
/// it preserves source isolation and only replaces the backdrop texture read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DestinationAccess {
    #[default]
    Sampled,
    CurrentPixelUnorm8,
}

impl DestinationAccess {
    pub(crate) fn for_samples(self, samples: u32) -> Self {
        if samples == 1 { self } else { Self::Sampled }
    }

    /// Independent masks and transition inputs retain their sampled execution.
    pub(crate) fn current_pixel(
        self,
        blend: BlendMode,
        source_over: bool,
        additional_input: bool,
    ) -> bool {
        code(blend).is_some_and(|operation| {
            self.current_pixel_code(operation, source_over, additional_input)
        })
    }

    pub(crate) fn current_pixel_code(
        self,
        operation: u32,
        source_over: bool,
        additional_input: bool,
    ) -> bool {
        self == Self::CurrentPixelUnorm8
            && source_over
            && !additional_input
            && (1..=11).contains(&operation)
    }

    pub(crate) fn backdrop(
        self,
        blend: BlendMode,
        source_over: bool,
        additional_input: bool,
    ) -> bool {
        blend != BlendMode::default() && !self.current_pixel(blend, source_over, additional_input)
    }
}

pub(crate) fn code(blend: BlendMode) -> Option<u32> {
    let blend = match blend {
        BlendMode::Standard(blend) => blend,
        BlendMode::Subtract => return Some(4),
        BlendMode::LinearBurn => return Some(11),
    };
    match (blend.mix, blend.compose) {
        (Mix::Normal, Compose::SrcOver) => Some(0),
        (Mix::Normal, Compose::PlusLighter) => Some(1),
        (Mix::Multiply, Compose::SrcOver) => Some(2),
        (Mix::Screen, Compose::SrcOver) => Some(3),
        (Mix::Overlay, Compose::SrcOver) => Some(5),
        (Mix::HardLight, Compose::SrcOver) => Some(6),
        (Mix::ColorDodge, Compose::SrcOver) => Some(7),
        (Mix::Lighten, Compose::SrcOver) => Some(8),
        (Mix::Difference, Compose::SrcOver) => Some(9),
        (Mix::SoftLight, Compose::SrcOver) => Some(10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_pixel_requires_one_quantized_source_and_preserves_other_inputs() {
        let current = DestinationAccess::CurrentPixelUnorm8;
        for blend in [
            Mix::Multiply.into(),
            Mix::Screen.into(),
            Mix::ColorDodge.into(),
            Mix::Overlay.into(),
            BlendMode::Subtract,
            BlendMode::LinearBurn,
        ] {
            assert!(current.current_pixel(blend, true, false));
            assert!(!current.backdrop(blend, true, false));
            for access in [DestinationAccess::Sampled, current.for_samples(4)] {
                assert!(access.backdrop(blend, true, false));
            }
            assert!(current.backdrop(blend, false, false));
            assert!(current.backdrop(blend, true, true));
        }
        assert!(!current.current_pixel(Default::default(), true, false));
        assert!(!current.backdrop(Default::default(), true, false));
    }
}
