//! Exact 8-bit linear-to-sRGB conversion for GXM capture output.
use std::sync::OnceLock;

fn channel(value: u8) -> u8 {
    let x = f32::from(value) / 255.;
    let s = if x <= 0.0031308 {
        12.92 * x
    } else {
        1.055 * x.powf(1. / 2.4) - 0.055
    };
    (s.clamp(0., 1.) * 255.).round() as u8
}

pub(crate) fn encode_rgba(bytes: &mut [u8]) {
    static TABLE: OnceLock<[u8; 256]> = OnceLock::new();
    let table = TABLE.get_or_init(|| std::array::from_fn(|index| channel(index as u8)));
    for pixel in bytes.as_chunks_mut::<4>().0 {
        pixel[0] = table[pixel[0] as usize];
        pixel[1] = table[pixel[1] as usize];
        pixel[2] = table[pixel[2] as usize];
        pixel[3] = 255;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_input_values_and_alpha_match_existing_formula() {
        let mut data: Vec<u8> = (0..=255).flat_map(|i| [i, 255 - i, i ^ 0x55, i]).collect();
        let mut expected = data.clone();
        for pixel in expected.as_chunks_mut::<4>().0 {
            for value in &mut pixel[..3] {
                *value = channel(*value);
            }
            pixel[3] = 255;
        }
        encode_rgba(&mut data);
        assert_eq!(data, expected);
    }
    #[test]
    #[ignore = "release-mode capture color conversion cost"]
    fn capture_conversion_cost() {
        use std::{hint::black_box, time::Instant};
        for [width, height] in [[320, 180], [960, 544]] {
            let source: Vec<u8> = (0..width * height * 4).map(|i| (i * 71) as u8).collect();
            let mut reference = source.clone();
            let mut output = source.clone();
            encode_rgba(&mut output);
            let iterations = 20;
            let start = Instant::now();
            for _ in 0..iterations {
                reference.copy_from_slice(&source);
                for pixel in black_box(&mut reference).as_chunks_mut::<4>().0 {
                    for value in &mut pixel[..3] {
                        *value = channel(*value);
                    }
                    pixel[3] = 255;
                }
            }
            let scalar = start.elapsed();
            let start = Instant::now();
            for _ in 0..iterations {
                output.copy_from_slice(&source);
                encode_rgba(black_box(&mut output));
            }
            let table = start.elapsed();
            assert_eq!(output, reference);
            let powf_calls = source
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|pixel| &pixel[..3])
                .filter(|&&value| value != 0)
                .count()
                * iterations;
            println!(
                "capture={width}x{height} iterations={iterations} powf_calls_old={powf_calls} table_init_powf_calls=255 old={scalar:?} table={table:?}"
            );
        }
    }
}
