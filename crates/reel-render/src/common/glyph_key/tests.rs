use super::*;
use reel_ui::{
    kurbo::{Affine, Stroke, Vec2},
    peniko::{Brush, Color, Style},
};

fn run(styled: bool) -> FrameGlyphRun {
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
        normalized_coords: if styled { vec![256; 16] } else { Vec::new() },
        style: if styled {
            Style::Stroke(Stroke::new(2.).with_dashes(0., [2., 3., 4., 5.]))
        } else {
            Style::Fill(Default::default())
        },
        glyphs: Vec::new(),
        brush: Brush::Solid(Color::WHITE),
        composite: Default::default(),
    }
}

#[test]
#[ignore = "CPU atlas-key lookup measurement; no graphics adapter"]
fn atlas_key_lookup_cost() {
    use std::{collections::HashMap, hint::black_box, time::Instant};
    for styled in [false, true] {
        let run = run(styled);
        let stored = Arc::new(RunKey::new(&run, 1.));
        let cache: HashMap<_, _> = (0..96)
            .map(|glyph| (Key::new(stored.clone(), glyph), glyph))
            .collect();
        let began = Instant::now();
        let mut checksum = 0u64;
        for _ in 0..2000 {
            let lookup = Arc::new(RunKey::new(black_box(&run), 1.));
            for index in 0..512 {
                let key = Key::new(lookup.clone(), index % 96);
                checksum += u64::from(*cache.get(black_box(&key)).unwrap());
            }
        }
        println!(
            "atlas_key styled={styled} frames=2000 glyphs_per_frame=512 elapsed_us={} run_key_bytes={} checksum={checksum}",
            began.elapsed().as_micros(),
            size_of::<RunKey>()
        );
    }
}

#[test]
fn hash_collisions_preserve_full_raster_identity() {
    use std::collections::HashMap;
    let original = run(true);
    let first = RunKey::new(&original, 1.);
    let mut changed = original.clone();
    changed.normalized_coords[15] += 1;
    let mut second = RunKey::new(&changed, 1.);
    second.hash = first.hash;
    let first = Key::new(Arc::new(first), 42);
    let second = Key::new(Arc::new(second), 42);
    assert!(first != second);
    let cache = HashMap::from([(first.clone(), 1), (second.clone(), 2)]);
    assert_eq!(cache.len(), 2);
    assert_eq!(cache[&first], 1);
    assert_eq!(cache[&second], 2);
    assert!(RunKey::new(&original, 1.) == RunKey::new(&original, 1.));
    assert!(RunKey::new(&original, 1.) != RunKey::new(&original, 2.));
}
