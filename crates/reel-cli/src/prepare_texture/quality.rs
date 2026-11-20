//! Select BC color fitting by displayed error while retaining its fixed GPU footprint.
use super::*;
use std::sync::LazyLock;

mod alpha;

struct Metrics {
    display_rmse_codes: f64,
    p99_abs_error_codes: u16,
    alpha_rmse_codes: f64,
    translucent_rmse_codes: f64,
    smooth_bias_rmse_codes: f64,
    smooth_gradient_rmse_codes: f64,
}

impl Metrics {
    fn needs_refinement(&self) -> bool {
        // Require at most one code of total or translucent RMSE, four codes
        // at the tail, and half a code of smooth-region bias.
        self.display_rmse_codes > 1.
            || self.p99_abs_error_codes > 4
            || self.smooth_bias_rmse_codes > 0.5
            || self.translucent_rmse_codes > 1.
    }

    fn score(&self) -> f64 {
        self.display_rmse_codes.powi(2)
            + self.smooth_bias_rmse_codes.powi(2)
            + self.smooth_gradient_rmse_codes.powi(2)
    }

    fn improves(&self, baseline: &Self) -> bool {
        if self.needs_refinement() != baseline.needs_refinement() {
            return !self.needs_refinement();
        }
        self.score() < baseline.score()
            && self.p99_abs_error_codes <= baseline.p99_abs_error_codes
            && self.translucent_rmse_codes <= baseline.translucent_rmse_codes
            && self.alpha_rmse_codes <= baseline.alpha_rmse_codes
    }
}

pub(crate) fn encode(
    linear: &image::Rgba32FImage,
    descriptor: TextureDescriptor,
) -> Result<(Vec<u8>, bool), String> {
    let [width, height] = descriptor.layout.storage_size;
    let format = if descriptor.format == TextureFormat::Bc1Rgb {
        rgbcx::Format::Bc1
    } else {
        rgbcx::Format::Bc3
    };
    let candidate = |quality| {
        let pixels = quantized_pixels(linear, descriptor, Dither::None);
        rgbcx::encode_parallel(width, height, pixels.as_raw(), format, quality)
            .map_err(|error| error.to_string())
    };
    let assess = |blocks: &[u8]| {
        let decoded =
            rgbcx::decode(width, height, blocks, format).map_err(|error| error.to_string())?;
        Ok::<_, String>(measure(linear, &decoded, width).finish())
    };
    let baseline = candidate(rgbcx::Quality::Balanced)?;
    let baseline_error = assess(&baseline)?;
    if !baseline_error.needs_refinement() {
        return Ok((baseline, true));
    }
    // High fitting keeps the same format and payload length. Keep the
    // baseline blocks until displayed colors, tails and translucent edges agree.
    let high = candidate(rgbcx::Quality::High)?;
    let high_error = assess(&high)?;
    if !high_error.needs_refinement() {
        return Ok((high, true));
    }
    let prefer_high = high_error.improves(&baseline_error);
    let best_error = if prefer_high {
        &high_error
    } else {
        &baseline_error
    };
    if format == rgbcx::Format::Bc3
        && let Some(mut adjusted) = alpha::refine(&baseline, linear, width)
    {
        let baseline_adjusted_error = assess(&adjusted)?;
        if !baseline_adjusted_error.needs_refinement() {
            return Ok((adjusted, true));
        }
        // Both RGB modes start with identical alpha. Fit it once, then compare
        // both color encodings after the new alpha has been applied.
        replace_color(&mut adjusted, &high);
        let high_adjusted_error = assess(&adjusted)?;
        if high_adjusted_error.improves(&baseline_adjusted_error) {
            if high_adjusted_error.improves(best_error) {
                return Ok((adjusted, !high_adjusted_error.needs_refinement()));
            }
        } else if baseline_adjusted_error.improves(best_error) {
            replace_color(&mut adjusted, &baseline);
            return Ok((adjusted, false));
        }
    }
    Ok((if prefer_high { high } else { baseline }, false))
}

fn replace_color(destination: &mut [u8], source: &[u8]) {
    for (dest, source) in destination
        .as_chunks_mut::<16>()
        .0
        .iter_mut()
        .zip(source.as_chunks::<16>().0)
    {
        dest[8..].copy_from_slice(&source[8..]);
    }
}

struct Stats {
    pixels: u64,
    display_squared: f64,
    alpha_squared: f64,
    translucent_squared: f64,
    translucent: u64,
    histogram: [u64; 256],
    smooth_bias_squared: f64,
    smooth: u64,
    gradient_squared: f64,
    pairs: u64,
}
impl Default for Stats {
    fn default() -> Self {
        Self {
            pixels: 0,
            display_squared: 0.,
            alpha_squared: 0.,
            translucent_squared: 0.,
            translucent: 0,
            histogram: [0; 256],
            smooth_bias_squared: 0.,
            smooth: 0,
            gradient_squared: 0.,
            pairs: 0,
        }
    }
}
impl Stats {
    fn finish(self) -> Metrics {
        let rmse = |sum: f64, count: u64| {
            if count == 0 {
                0.
            } else {
                (sum / count as f64).sqrt()
            }
        };
        let threshold = (self.pixels * 99).div_ceil(100);
        let mut count = 0;
        let mut p99 = 0;
        for (index, bin) in self.histogram.into_iter().enumerate() {
            count += bin;
            if count >= threshold {
                p99 = index as u16;
                break;
            }
        }
        Metrics {
            display_rmse_codes: rmse(self.display_squared, self.pixels * 9),
            p99_abs_error_codes: p99,
            alpha_rmse_codes: rmse(self.alpha_squared, self.pixels),
            translucent_rmse_codes: rmse(self.translucent_squared, self.translucent * 9),
            smooth_bias_rmse_codes: rmse(self.smooth_bias_squared, self.smooth),
            smooth_gradient_rmse_codes: rmse(self.gradient_squared, self.pairs),
        }
    }
}

static DISPLAY: LazyLock<Box<[f32]>> = LazyLock::new(|| {
    (0..=65535)
        .map(|index| {
            let value = index as f32 / 65535.;
            255. * if value <= 0.0031308 {
                12.92 * value
            } else {
                1.055 * value.powf(1. / 2.4) - 0.055
            }
        })
        .collect()
});

fn display(value: f32) -> f64 {
    let index = value.clamp(0., 1.) * 65535.;
    let lower = index as usize;
    let start = DISPLAY[lower];
    f64::from(start + (DISPLAY[(lower + 1).min(65535)] - start) * index.fract())
}

#[derive(Clone, Copy, Default)]
struct Region {
    reference: f64,
    decoded: f64,
    smooth: bool,
}

pub(super) fn scratch_bytes(width: u32) -> u128 {
    u128::from(width.div_ceil(8)) * size_of::<Region>() as u128
}

fn measure(reference: &image::Rgba32FImage, decoded: &[u8], stride: u32) -> Stats {
    measure_samples(reference, |x, y| {
        let at = (y as usize * stride as usize + x as usize) * 4;
        decoded[at..at + 4].try_into().unwrap()
    })
}

pub(super) fn indexed_acceptable(
    reference: &image::Rgba32FImage,
    descriptor: TextureDescriptor,
    payload: &[u8],
) -> bool {
    let stride = descriptor.layout.storage_size[0] as usize;
    let palette = payload[descriptor.palette_offset().unwrap() as usize..]
        .as_chunks::<4>()
        .0;
    !measure_samples(reference, |x, y| {
        palette[usize::from(payload[y as usize * stride + x as usize])]
    })
    .finish()
    .needs_refinement()
}

fn measure_samples(reference: &image::Rgba32FImage, sample: impl Fn(u32, u32) -> [u8; 4]) -> Stats {
    // Compare displayed colors after compositing, including translucent edges.
    // Region means capture banding that a pixel-only average can hide. Padding
    // contributes no samples because its repeated edge is outside the content.
    let mut stats = Stats::default();
    let mut above = vec![Region::default(); reference.width().div_ceil(8) as usize];
    for block_y in (0..reference.height()).step_by(8) {
        let mut left = Region::default();
        for (column, block_x) in (0..reference.width()).step_by(8).enumerate() {
            let mut region = Region::default();
            let mut minimum = f64::INFINITY;
            let mut maximum = f64::NEG_INFINITY;
            let mut count = 0;
            for y in block_y..(block_y + 8).min(reference.height()) {
                for x in block_x..(block_x + 8).min(reference.width()) {
                    // Compare the extra error against the pixels the
                    // RGBA8 fallback would actually present, including roundoff.
                    let original = quantize(reference.get_pixel(x, y).0, Dither::None, [x, y])
                        .map(|value| f32::from(value) / 255.);
                    let sample = sample(x, y).map(|value| f32::from(value) / 255.);
                    let mut squared = 0.;
                    let mut max_error = 0f64;
                    let mut luma = [0.; 2];
                    for background in [0., 0.18, 1.] {
                        for (channel, weight) in [0.2126, 0.7152, 0.0722].into_iter().enumerate() {
                            let expected =
                                display(original[channel] + background * (1. - original[3]));
                            // Both renderers clamp BC3's independently fitted
                            // RGB to alpha before compositing the sampled color.
                            let actual = display(
                                sample[channel].min(sample[3]) + background * (1. - sample[3]),
                            );
                            let error = actual - expected;
                            squared += error * error;
                            max_error = max_error.max(error.abs());
                            if background == 0.18 {
                                luma[0] += expected * weight;
                                luma[1] += actual * weight;
                            }
                        }
                    }
                    stats.display_squared += squared;
                    stats.alpha_squared += f64::from((sample[3] - original[3]) * 255.).powi(2);
                    if original[3] > 0. && original[3] < 1. {
                        stats.translucent += 1;
                        stats.translucent_squared += squared;
                    }
                    stats.histogram[(max_error.ceil() as usize).min(255)] += 1;
                    stats.pixels += 1;
                    minimum = minimum.min(luma[0]);
                    maximum = maximum.max(luma[0]);
                    region.reference += luma[0];
                    region.decoded += luma[1];
                    count += 1;
                }
            }
            region.reference /= f64::from(count);
            region.decoded /= f64::from(count);
            region.smooth = (0.25..=8.).contains(&(maximum - minimum));
            if region.smooth {
                stats.smooth += 1;
                stats.smooth_bias_squared += (region.decoded - region.reference).powi(2);
                for other in [left, above[column]] {
                    if other.smooth && (region.reference - other.reference).abs() <= 8. {
                        stats.pairs += 1;
                        stats.gradient_squared += ((region.decoded - other.decoded)
                            - (region.reference - other.reference))
                            .powi(2);
                    }
                }
            }
            above[column] = region;
            left = region;
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn alpha_error_is_measured_after_compositing_and_padding_is_excluded() {
        let reference = image::Rgba32FImage::from_pixel(1, 1, image::Rgba([0.25, 0.25, 0.25, 0.5]));
        let exact = measure(&reference, &[64, 64, 64, 128, 255, 0, 0, 0], 2);
        let wrong = measure(&reference, &[64, 64, 64, 255, 0, 0, 0, 0], 2);
        assert_eq!(exact.pixels, 1);
        assert_eq!(exact.display_squared, 0.);
        assert_eq!(wrong.translucent, 1);
        assert!(wrong.display_squared > exact.display_squared * 100.);
        assert!(wrong.alpha_squared > exact.alpha_squared * 100.);
        let clamped = measure(&reference, &[128, 80, 64, 128], 1);
        let overshot = measure(&reference, &[255, 80, 64, 128], 1);
        assert_eq!(overshot.display_squared, clamped.display_squared);
        assert_eq!(overshot.translucent_squared, clamped.translucent_squared);
        assert_eq!(overshot.histogram, clamped.histogram);
        assert_eq!(overshot.alpha_squared, exact.alpha_squared);
    }

    #[test]
    fn selected_blocks_preserve_format_and_display_quality() {
        for format in [Format::Bc1, Format::Bc3] {
            let original = RgbaImage::from_fn(64, 32, |x, y| {
                image::Rgba([
                    (16 + x * 2) as u8,
                    (32 + y * 3) as u8,
                    (48 + x + y) as u8,
                    if matches!(format, Format::Bc3) {
                        (16 + x * 3) as u8
                    } else {
                        255
                    },
                ])
            });
            let (descriptor, linear) = prepare_linear(
                original.into(),
                Options {
                    format,
                    ..Default::default()
                },
                1.,
            )
            .unwrap();
            let codec = if matches!(format, Format::Bc1) {
                rgbcx::Format::Bc1
            } else {
                rgbcx::Format::Bc3
            };
            let pixels = quantized_pixels(&linear, descriptor, Dither::None);
            let baseline =
                rgbcx::encode(64, 32, pixels.as_raw(), codec, rgbcx::Quality::Balanced).unwrap();
            let (selected, _) = encode(&linear, descriptor).unwrap();
            assert_eq!(selected.len(), baseline.len());
            let assess = |blocks: &[u8]| {
                measure(&linear, &rgbcx::decode(64, 32, blocks, codec).unwrap(), 64).finish()
            };
            let before = assess(&baseline);
            let after = assess(&selected);
            let serial = if before.needs_refinement() {
                let refined =
                    rgbcx::encode(64, 32, pixels.as_raw(), codec, rgbcx::Quality::High).unwrap();
                if assess(&refined).improves(&before) {
                    refined
                } else {
                    baseline
                }
            } else {
                baseline
            };
            if matches!(codec, rgbcx::Format::Bc1) {
                assert_eq!(selected, serial);
            } else if selected != serial {
                assert!(after.improves(&assess(&serial)));
            }
            assert!(after.score() <= before.score());
            assert!(after.p99_abs_error_codes <= before.p99_abs_error_codes);
            assert!(after.translucent_rmse_codes <= before.translucent_rmse_codes);
            assert!(after.alpha_rmse_codes <= before.alpha_rmse_codes);
        }
    }

    #[test]
    fn bc3_alpha_refines_soft_edges_without_changing_color_blocks() {
        let edge = [
            0, 0, 14, 51, 0, 14, 51, 105, 14, 51, 105, 170, 51, 105, 170, 255,
        ];
        let original = RgbaImage::from_fn(4, 4, |x, y| {
            image::Rgba([0, 0, 0, edge[(y * 4 + x) as usize]])
        });
        let (descriptor, linear) = prepare_linear(
            original.into(),
            Options {
                format: Format::Bc3,
                ..Default::default()
            },
            1.,
        )
        .unwrap();
        let pixels = quantized_pixels(&linear, descriptor, Dither::None);
        let baseline = rgbcx::encode(
            4,
            4,
            pixels.as_raw(),
            rgbcx::Format::Bc3,
            rgbcx::Quality::High,
        )
        .unwrap();
        let (selected, _) = encode(&linear, descriptor).unwrap();
        assert_eq!(selected.len(), baseline.len());
        assert_eq!(selected[8..], baseline[8..]);
        assert!(selected[0] < selected[1]);
        let decode = |blocks: &[u8]| rgbcx::decode(4, 4, blocks, rgbcx::Format::Bc3).unwrap();
        let decoded = decode(&selected);
        assert_eq!(decoded[3], 0);
        assert_eq!(decoded[63], 255);
        let before = measure(&linear, &decode(&baseline), 4).finish();
        let after = measure(&linear, &decoded, 4).finish();
        assert!(after.improves(&before));
        assert!(after.alpha_rmse_codes < 0.35 * before.alpha_rmse_codes);
    }
}
