use reel_ui::{frame::FrameGlyphRun, resources::ResourceHandle};
use std::sync::Arc;
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    run: Arc<RunKey>,
    glyph: u32,
}
#[derive(PartialEq, Eq, Hash)]
pub(crate) struct RunKey {
    font: ResourceHandle,
    index: u32,
    size: u32,
    density: u64,
    hint: bool,
    coords: Vec<i16>,
    transform: Option<[u64; 6]>,
    embolden: [u64; 2],
    style: StyleKey,
}
#[derive(PartialEq, Eq, Hash)]
enum StyleKey {
    Fill(bool),
    Stroke {
        width: u64,
        join: u8,
        miter: u64,
        caps: [u8; 2],
        offset: u64,
        dashes: Vec<u64>,
    },
}
impl RunKey {
    pub fn new(run: &FrameGlyphRun, density: f64) -> Self {
        Self {
            font: run.font,
            index: run.font_index,
            size: run.font_size.to_bits(),
            density: density.to_bits(),
            hint: run.hint,
            coords: run.normalized_coords.to_vec(),
            transform: run.glyph_transform.map(|t| t.as_coeffs().map(f64::to_bits)),
            embolden: [run.font_embolden.x.to_bits(), run.font_embolden.y.to_bits()],
            style: match &run.style {
                reel_ui::peniko::Style::Fill(fill) => {
                    StyleKey::Fill(*fill == reel_ui::peniko::Fill::EvenOdd)
                }
                reel_ui::peniko::Style::Stroke(s) => StyleKey::Stroke {
                    width: s.width.to_bits(),
                    join: s.join as u8,
                    miter: s.miter_limit.to_bits(),
                    caps: [s.start_cap as u8, s.end_cap as u8],
                    offset: s.dash_offset.to_bits(),
                    dashes: s.dash_pattern.iter().map(|v| v.to_bits()).collect(),
                },
            },
        }
    }
}
impl Key {
    pub fn new(run: Arc<RunKey>, glyph: u32) -> Self {
        Self { run, glyph }
    }
}
