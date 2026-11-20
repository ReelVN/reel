//! Refine the scalar plane of difficult BC3 images without changing their RGB blocks.
use super::{Dither, quantize};

pub(super) fn refine(
    blocks: &[u8],
    linear: &image::Rgba32FImage,
    storage_width: u32,
) -> Option<Vec<u8>> {
    let blocks_per_row = storage_width / 4;
    let mut refined = None;
    for (index, block) in blocks.as_chunks::<16>().0.iter().enumerate() {
        if block[0] == block[1] {
            // rgbcx fixes the alpha endpoints to the block minimum and maximum.
            // Equal endpoints therefore identify a constant block.
            continue;
        }
        let x = index as u32 % blocks_per_row * 4;
        let y = index as u32 / blocks_per_row * 4;
        let alpha = std::array::from_fn(|texel| {
            let x = (x + texel as u32 % 4).min(linear.width() - 1);
            let y = (y + texel as u32 / 4).min(linear.height() - 1);
            quantize(linear.get_pixel(x, y).0, Dither::None, [x, y])[3]
        });
        if let Some(plane) = fit(alpha, block.first_chunk::<8>().unwrap()) {
            let bytes = refined.get_or_insert_with(|| blocks.to_vec());
            let start = index * 16;
            bytes[start..start + 8].copy_from_slice(&plane);
        }
    }
    refined
}

fn fit(alpha: [u8; 16], block: &[u8; 8]) -> Option<[u8; 8]> {
    let original_palette = palette(block[0], block[1]);
    let mut indices = [0; 8];
    indices[..6].copy_from_slice(&block[2..8]);
    let indices = u64::from_le_bytes(indices);
    let original_error = alpha
        .iter()
        .enumerate()
        .map(|(index, &value)| {
            let selected = ((indices >> (index * 3)) & 7) as usize;
            squared_error(value, original_palette[selected])
        })
        .sum::<u32>();
    if original_error == 0 {
        return None;
    }

    let min = *alpha.iter().min().unwrap();
    let max = *alpha.iter().max().unwrap();
    let interior = || {
        alpha
            .iter()
            .copied()
            .filter(|value| (1..255).contains(value))
    };
    let mut best_error = original_error;
    let mut best = None;
    // Ascending endpoints reserve two selectors for exact 0 and 255, which lets
    // the other six values represent only the interior of a translucent edge.
    let center = [
        (max, min, false),
        (interior().min().unwrap(), interior().max().unwrap(), true),
    ];
    const DELTAS: [i16; 9] = [0, -1, 1, -2, 2, -4, 4, -8, 8];
    for (a, b, has_extremes) in center {
        for delta_a in DELTAS {
            let a = (i16::from(a) + delta_a).clamp(0, 255) as u8;
            for delta_b in DELTAS {
                let b = (i16::from(b) + delta_b).clamp(0, 255) as u8;
                if has_extremes != (a <= b) {
                    continue;
                }
                if !has_extremes && (min == 0 && b != 0 || max == 255 && a != 255) {
                    continue;
                }
                if let Some((error, indices)) = evaluate(&alpha, palette(a, b), best_error) {
                    best_error = error;
                    let mut data = [a, b, 0, 0, 0, 0, 0, 0];
                    data[2..].copy_from_slice(&indices.to_le_bytes()[..6]);
                    best = Some(data);
                }
                if best_error == 0 {
                    return best;
                }
            }
        }
    }
    best
}

fn palette(a: u8, b: u8) -> [u8; 8] {
    let mut samples = [a, b, 0, 0, 0, 0, 0, 255];
    let a = u16::from(a);
    let b = u16::from(b);
    let divisor = if a > b { 7 } else { 5 };
    for step in 1..divisor {
        samples[usize::from(step) + 1] = ((a * (divisor - step) + b * step) / divisor) as u8;
    }
    samples
}

fn evaluate(alpha: &[u8; 16], palette: [u8; 8], limit: u32) -> Option<(u32, u64)> {
    let mut error = 0;
    let mut indices = 0;
    for (index, &value) in alpha.iter().enumerate() {
        let (selector, texel_error) = palette
            .iter()
            .enumerate()
            .map(|(selector, &sample)| (selector, squared_error(value, sample)))
            .min_by_key(|&(_, error)| error)
            .unwrap();
        error += texel_error;
        if error >= limit {
            return None;
        }
        indices |= (selector as u64) << (index * 3);
    }
    Some((error, indices))
}

fn squared_error(reference: u8, actual: u8) -> u32 {
    u32::from(reference.abs_diff(actual)).pow(2)
}
