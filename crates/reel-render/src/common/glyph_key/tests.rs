use super::*;
use reel_ui::{
    kurbo::{Affine, Stroke, Vec2},
    peniko::{Brush, Color, Style},
};

fn run() -> FrameGlyphRun {
    FrameGlyphRun {
        font: ResourceHandle {
            registry: 1,
            slot: 0,
            generation: 1,
        },
        font_index: 0,
        transform: Affine::IDENTITY,
        glyph_transform: None,
        brush_transform: None,
        font_size: 24.,
        font_embolden: Vec2::ZERO,
        hint: true,
        normalized_coords: vec![256; 16],
        style: Style::Stroke(Stroke::new(2.).with_dashes(0., [2., 3., 4., 5.])),
        glyphs: Vec::new(),
        brush: Brush::Solid(Color::WHITE),
        composite: Default::default(),
    }
}

#[test]
fn hash_collisions_preserve_full_raster_identity() {
    use std::collections::HashMap;
    let original = run();
    let first = RunKey::new(&original, 1.);
    let mut changed = original.clone();
    changed.normalized_coords[15] += 1;
    let mut second = RunKey::new(&changed, 1.);
    second.hash = first.hash;
    let first = Key {
        run: Arc::new(first),
        glyph: 42,
    };
    let second = Key {
        run: Arc::new(second),
        glyph: 42,
    };
    assert!(first != second);
    let cache = HashMap::from([(first.clone(), 1), (second.clone(), 2)]);
    assert_eq!(cache.len(), 2);
    assert_eq!(cache[&first], 1);
    assert_eq!(cache[&second], 2);
    assert!(RunKey::new(&original, 1.) == RunKey::new(&original, 1.));
    assert!(RunKey::new(&original, 1.) != RunKey::new(&original, 2.));
}
