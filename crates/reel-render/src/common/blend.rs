//! Shared composite shader operation codes. Keep both shader dispatches in sync.
use reel_ui::frame::BlendMode;
use reel_ui::peniko::{Compose, Mix};

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
