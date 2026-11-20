use reel_ui::{frame::FrameGlyphRun, resources::ResourceHandle};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::Arc,
};
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    run: Arc<RunKey>,
    glyph: u32,
}
#[derive(PartialEq, Eq)]
pub(crate) struct RunKey {
    hash: u64,
    fields: RunFields,
}
#[derive(PartialEq, Eq, Hash)]
struct RunFields {
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
        let fields = RunFields {
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
        };
        let mut hasher = DefaultHasher::new();
        fields.hash(&mut hasher);
        Self {
            hash: hasher.finish(),
            fields,
        }
    }
}
impl Hash for RunKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Each glyph shares this digest; equality still compares every raster field.
        state.write_u64(self.hash);
    }
}
impl Key {
    pub fn for_run(run: &FrameGlyphRun, density: f64) -> Self {
        Self {
            run: Arc::new(RunKey::new(run, density)),
            glyph: 0,
        }
    }

    /// Reuse the run's owned key for lookups. Clone only when storing a key.
    pub fn lookup(&mut self, glyph: u32) -> &Self {
        self.glyph = glyph;
        self
    }
}

#[cfg(test)]
mod tests;
