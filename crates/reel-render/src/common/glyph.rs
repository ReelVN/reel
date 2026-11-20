//! Consumes shaped glyph IDs/positions; this module never invokes Swash shaping.
use super::GpuError;
#[cfg(test)]
use reel_ui::frame::FrameDrawing;
use reel_ui::frame::{FrameGlyphRun, RecordLocation};
use reel_ui::resources::ResourceView;
use reel_ui::{kurbo, peniko};
use swash::{
    scale::{ScaleContext, Scaler, outline::Outline},
    zeno,
};

mod run;
mod stroke;
pub(crate) use run::RasterRun;
#[cfg(feature = "wgpu")]
pub(crate) use run::Rasterizers;
#[path = "synthesis.rs"]
mod synthesis;

#[derive(Default)]
pub(crate) struct Rasterizer {
    scale: ScaleContext,
    outline: Outline,
    scratch: zeno::Scratch,
    // Swash's convenience constructor creates a fresh cache identity each time.
    // Keep the last face identity so adjacent glyphs share its parsed tables.
    face: Option<(
        reel_ui::resources::ResourceHandle,
        u32,
        u32,
        swash::CacheKey,
    )>,
}
pub struct GlyphBitmap {
    pub x: f64,
    pub y: f64,
    pub size: [u32; 2],
    pub alpha: Vec<u8>,
    pub(super) _storage: reel_core::budget::BudgetTicket,
}

impl GlyphBitmap {
    /// Visit alpha rows inside an already-zeroed one-pixel border. The caller
    /// owns and accounts for its row stride and ordinary or write-only storage.
    pub(crate) fn padded_rows(&self, stride: usize, mut write: impl FnMut(usize, &[u8])) {
        let width = self.size[0] as usize;
        for (y, source) in self.alpha.chunks_exact(width).enumerate() {
            let start = (y + 1) * stride + 1;
            write(start, source);
        }
    }
}

#[derive(Clone)]
pub struct RasterLimits {
    pub available: u64,
    pub max_dimension: u32,
    pub budget: reel_core::budget::BudgetPool,
}

/// Half-octave densities cover the physical magnification while keeping animated
/// transforms on reusable atlas tiers, including views smaller than author units.
pub(crate) fn raster_density(transform: kurbo::Affine) -> f64 {
    let [a, b, c, d, _, _] = transform.as_coeffs();
    let magnification = ((a + d).hypot(b - c) + (a - d).hypot(b + c)) * 0.5;
    if magnification > 0. {
        2_f64.powf((magnification.log2() * 2.).ceil() * 0.5)
    } else {
        1.
    }
}

#[cfg(test)]
mod density_tests {
    use super::*;

    #[test]
    fn physical_density_covers_scaled_and_sheared_views_without_whole_octave_waste() {
        use kurbo::Affine;
        assert_eq!(raster_density(Affine::scale(0.5)), 0.5);
        assert_eq!(raster_density(Affine::scale(0.25)), 0.25);
        assert_eq!(raster_density(Affine::translate((123., 42.))), 1.);
        assert_eq!(
            raster_density(Affine::scale(1.25)),
            std::f64::consts::SQRT_2
        );
        assert_eq!(
            raster_density(Affine::scale(1.3)),
            raster_density(Affine::scale(1.25))
        );
        assert_eq!(raster_density(Affine::scale_non_uniform(0.5, 0.25)), 0.5);
        for transform in [
            Affine::scale(0.35),
            Affine::scale(1.25),
            Affine::scale_non_uniform(1.5, 0.75),
            Affine::rotate(0.7) * Affine::scale_non_uniform(0.5, 0.25),
            Affine::new([0.5, 0.25, 0.5, 1., 0., 0.]),
        ] {
            let density = raster_density(transform);
            let greatest = (0..360)
                .map(|angle| {
                    let theta = f64::from(angle).to_radians();
                    let point = transform * kurbo::Point::new(theta.cos(), theta.sin());
                    point.x.hypot(point.y)
                })
                .fold(0., f64::max);
            assert!(density + 1e-12 >= greatest);
            assert!(density <= greatest * std::f64::consts::SQRT_2 + 1e-5);
        }
    }
}

#[cfg(test)]
pub fn rasterize(
    context: &mut Rasterizer,
    frame: &FrameDrawing,
    run: &FrameGlyphRun,
    density: f64,
    index: usize,
    at: RecordLocation,
    limits: RasterLimits,
) -> Result<Option<GlyphBitmap>, GpuError> {
    RasterRun::new(frame, run, density, at)?.glyph(context, index, limits)
}

struct PreparedRasterizer<'a> {
    scaler: Scaler<'a>,
    outline: &'a mut Outline,
    scratch: &'a mut zeno::Scratch,
}

impl Rasterizer {
    fn prepare<'a>(
        &'a mut self,
        resource: &'a reel_ui::resources::ResourceLease,
        run: &FrameGlyphRun,
        density: f64,
    ) -> Result<PreparedRasterizer<'a>, &'static str> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::GlyphOutline);
        let ResourceView::Font(font) = resource.view() else {
            return Err("font resource type");
        };
        // Blob identity survives fallback-font switches and resource re-registration.
        // The collection index distinguishes faces sharing the same immutable bytes.
        let scaler_id = [font.id(), u64::from(run.font_index)];
        let (offset, key) = match self.face {
            Some((handle, index, offset, key)) if handle == run.font && index == run.font_index => {
                (offset, key)
            }
            _ => {
                let face = swash::FontRef::from_index(font.data(), run.font_index as usize)
                    .ok_or("font collection index")?;
                self.face = Some((run.font, run.font_index, face.offset, face.key));
                (face.offset, face.key)
            }
        };
        let font = swash::FontRef {
            data: font.data(),
            offset,
            key,
        };
        let scaler = self
            .scale
            .builder_with_id(font, scaler_id)
            .size(run.font_size * density as f32)
            .hint(run.hint)
            .normalized_coords(run.normalized_coords.iter().copied())
            .build();
        Ok(PreparedRasterizer {
            scaler,
            outline: &mut self.outline,
            scratch: &mut self.scratch,
        })
    }
}

fn rasterize_glyph(
    prepared: &mut PreparedRasterizer<'_>,
    run: &FrameGlyphRun,
    density: f64,
    index: usize,
    at: RecordLocation,
    limits: RasterLimits,
) -> Result<Option<GlyphBitmap>, GpuError> {
    let outline_timing = reel_core::profile::Scope::new(reel_core::profile::Phase::GlyphOutline);
    let glyph = &run.glyphs[index];
    let id = u16::try_from(glyph.id).map_err(|_| GpuError::Unsupported {
        at,
        record: "glyph id",
    })?;
    if prepared.scaler.scale_color_outline(id).is_some() {
        return Err(GpuError::Unsupported {
            at,
            record: "color glyph layers",
        });
    }
    let outline = &mut *prepared.outline;
    if !prepared.scaler.scale_outline_into(id, outline) {
        return Err(GpuError::Unsupported {
            at,
            record: "missing glyph outline",
        });
    }
    if outline.points().is_empty() {
        return Ok(None);
    }
    if run.font_embolden != kurbo::Vec2::ZERO {
        synthesis::embolden(
            outline,
            (run.font_embolden.x * density) as f32,
            (run.font_embolden.y * density) as f32,
        );
    }
    if let Some(transform) = run.glyph_transform {
        let [a, b, c, d, e, f] = transform.as_coeffs().map(|v| v as f32);
        outline.transform(&zeno::Transform::new(
            a,
            b,
            c,
            d,
            e * density as f32,
            f * density as f32,
        ));
    }
    if outline
        .points()
        .iter()
        .any(|p| !p.x.is_finite() || !p.y.is_finite())
    {
        return Err(GpuError::Unsupported {
            at,
            record: "glyph raster coordinates",
        });
    }
    let dashes;
    drop(outline_timing);
    let style = match &run.style {
        peniko::Style::Fill(fill) => zeno::Style::Fill(match fill {
            peniko::Fill::NonZero => zeno::Fill::NonZero,
            peniko::Fill::EvenOdd => zeno::Fill::EvenOdd,
        }),
        peniko::Style::Stroke(stroke) => {
            dashes = stroke
                .dash_pattern
                .iter()
                .map(|v| (*v * density) as f32)
                .collect::<Vec<_>>();
            let width = (stroke.width * density) as f32;
            let miter_limit = stroke.miter_limit as f32;
            let offset = (stroke.dash_offset * density) as f32;
            if !width.is_finite()
                || width < 0.
                || !offset.is_finite()
                || !miter_limit.is_finite()
                || miter_limit < 0.
                || dashes.iter().any(|v| !v.is_finite() || *v < 0.)
            {
                return Err(GpuError::Unsupported {
                    at,
                    record: "glyph stroke parameters",
                });
            }
            if width == 0. {
                return Ok(None);
            }
            zeno::Style::Stroke(zeno::Stroke {
                width,
                join: match stroke.join {
                    kurbo::Join::Bevel => zeno::Join::Bevel,
                    kurbo::Join::Round => zeno::Join::Round,
                    kurbo::Join::Miter if miter_limit < 1. => zeno::Join::Bevel,
                    kurbo::Join::Miter => zeno::Join::Miter,
                },
                miter_limit,
                start_cap: cap(stroke.start_cap),
                end_cap: cap(stroke.end_cap),
                dashes: if dashes.iter().any(|v| *v > 0.) {
                    &dashes
                } else {
                    &[]
                },
                offset,
                scale: true,
            })
        }
    };
    if matches!(style, zeno::Style::Stroke(_)) {
        let stroke_timing = reel_core::profile::Scope::new(reel_core::profile::Phase::GlyphStroke);
        // Zeno otherwise expands the stroke once for bounds and again for pixels.
        let mut path = stroke::StrokePath::new(&limits.budget, limits.available)?;
        let fill = prepared
            .scratch
            .apply(outline.path(), style, None, &mut path);
        path.finish()?;
        let available = limits.available.saturating_sub(path.bytes());
        drop(stroke_timing);
        raster_mask(
            path.commands(),
            fill.into(),
            prepared.scratch,
            None,
            at,
            RasterLimits {
                available,
                ..limits
            },
        )
    } else {
        raster_mask(
            outline.path(),
            style,
            prepared.scratch,
            Some(outline.bounds()),
            at,
            limits,
        )
    }
}

fn raster_mask(
    path: impl zeno::PathData,
    style: zeno::Style<'_>,
    scratch: &mut zeno::Scratch,
    bounds: Option<zeno::Bounds>,
    at: RecordLocation,
    RasterLimits {
        available,
        max_dimension,
        budget,
    }: RasterLimits,
) -> Result<Option<GlyphBitmap>, GpuError> {
    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::GlyphCoverage);
    let mut mask = zeno::Mask::with_scratch(path, scratch);
    // Both Swash and Zeno use the control-point bounds for fill paths.
    let min = bounds.map(|bounds| {
        let min = bounds.min.floor();
        let max = bounds.max.ceil();
        mask.size((max.x - min.x) as u32, (max.y - min.y) as u32)
            .render_offset(zeno::Vector::new(-min.x, -min.y));
        min
    });
    let mut size = [0; 2];
    mask.format(zeno::Format::Alpha)
        .origin(zeno::Origin::BottomLeft)
        .style(style)
        .inspect(|_, width, height| size = [width, height]);
    if size.contains(&0) {
        return Ok(None);
    }
    if size.iter().any(|v| *v > max_dimension) {
        return Err(GpuError::Unsupported {
            at,
            record: "glyph raster exceeds device texture dimensions",
        });
    }
    let cost = super::resources::upload_cost(size)?;
    if cost > available {
        return Err(GpuError::Budget {
            domain: "glyph raster",
            requested: cost,
            available,
        });
    }
    let bytes = u64::from(size[0]) * u64::from(size[1]);
    let storage = budget.reserve(bytes).map_err(|_| GpuError::Budget {
        domain: "glyph coverage",
        requested: bytes,
        available,
    })?;
    let mut alpha = vec![0; size[0] as usize * size[1] as usize];
    let placement = mask.render_into(&mut alpha, None);
    let (left, top) = min.map_or((placement.left, placement.top), |min| {
        (min.x as i32, (min.y + placement.height as f32) as i32)
    });
    Ok(Some(GlyphBitmap {
        x: f64::from(left),
        y: -f64::from(top),
        size,
        alpha,
        _storage: storage,
    }))
}

fn cap(cap: kurbo::Cap) -> zeno::Cap {
    match cap {
        kurbo::Cap::Butt => zeno::Cap::Butt,
        kurbo::Cap::Square => zeno::Cap::Square,
        kurbo::Cap::Round => zeno::Cap::Round,
    }
}

#[cfg(test)]
#[test]
fn fill_bounds_preserve_glyph_placement_and_coverage() {
    use reel_ui::core::Widget;

    let bytes = include_bytes!(
        "../../../../examples/author_arcade/resources/fonts/AtkinsonHyperlegible-Regular.ttf"
    );
    let font = swash::FontRef::from_index(bytes, 0).unwrap();
    let mut scale = ScaleContext::new();
    let mut scaler = scale.builder(font).size(29.4).hint(true).build();
    let mut scratch = zeno::Scratch::new();
    let limits = RasterLimits {
        available: 1 << 20,
        max_dimension: 2048,
        budget: super::renderer::Budgets::new(Default::default()).transient,
    };
    let at = RecordLocation {
        fragment: 0,
        command: 0,
        mask_depth: 0,
        owner: reel_ui::widgets::Label::new("").prepare().id(),
    };
    for glyph in ['A', 'g', 'é', '0', ':'] {
        let mut outline = scaler.scale_outline(font.charmap().map(glyph)).unwrap();
        synthesis::embolden(&mut outline, 0.92, 1.2);
        outline.transform(&zeno::Transform::new(-1., 0.1, -0.13, 0.77, 1.1, -3.53));
        for fill in [zeno::Fill::NonZero, zeno::Fill::EvenOdd] {
            let mut render = |bounds| {
                raster_mask(
                    outline.path(),
                    fill.into(),
                    &mut scratch,
                    bounds,
                    at,
                    limits.clone(),
                )
                .unwrap()
                .unwrap()
            };
            let expected = render(None);
            let actual = render(Some(outline.bounds()));
            assert_eq!(
                (actual.x, actual.y, actual.size),
                (expected.x, expected.y, expected.size)
            );
            assert_eq!(actual.alpha, expected.alpha);
        }
    }
}

#[cfg(test)]
#[test]
fn glyph_alpha_padding_preserves_coverage_and_zero_border() {
    let budgets = super::renderer::Budgets::new(super::GpuLimits::default());
    let bitmap = GlyphBitmap {
        x: 0.,
        y: 0.,
        size: [2, 2],
        alpha: vec![0, 255, 17, 89],
        _storage: budgets.transient.reserve(4).unwrap(),
    };
    for stride in [4, 8, 256] {
        let mut output = vec![0; 4 * stride];
        bitmap.padded_rows(stride, |start, source| {
            output[start..start + source.len()].copy_from_slice(source)
        });
        for (index, &value) in output.iter().enumerate() {
            let (x, y) = (index % stride, index / stride);
            let expected = if (1..=2).contains(&x) && (1..=2).contains(&y) {
                bitmap.alpha[(y - 1) * 2 + x - 1]
            } else {
                0
            };
            assert_eq!(value, expected, "stride={stride} position={x},{y}");
        }
    }
}

#[cfg(test)]
mod stroke_tests {
    use super::*;
    use std::hint::black_box;
    use std::time::Instant;

    fn bitmap(
        path: impl zeno::PathData,
        style: zeno::Style<'_>,
        scratch: &mut zeno::Scratch,
    ) -> (Vec<u8>, [i64; 4]) {
        let mut mask = zeno::Mask::with_scratch(path, scratch);
        let mut bytes = 0;
        mask.origin(zeno::Origin::BottomLeft)
            .style(style)
            .inspect(|_, w, h| bytes = (w * h) as usize);
        let mut data = vec![0; bytes];
        let at = mask.render_into(&mut data, None);
        (
            data,
            [
                at.left as i64,
                at.top as i64,
                at.width as i64,
                at.height as i64,
            ],
        )
    }

    #[test]
    #[ignore = "CPU glyph stroke measurement; set REEL_R01_TEST_FONT to an installed CJK font"]
    fn stroke_expansion_pixels_and_cost() {
        let bytes = std::fs::read(std::env::var("REEL_R01_TEST_FONT").unwrap()).unwrap();
        let font = swash::FontRef::from_index(&bytes, 0).unwrap();
        let mut scale = ScaleContext::new();
        let mut scaler = scale.builder(font).size(40.).hint(true).build();
        let outlines: Vec<_> = "历史文字设置音量自动播放存档读取标题返回继续游戏ABC123，。!?"
            .chars()
            .filter_map(|ch| scaler.scale_outline(font.charmap().map(ch)))
            .collect();
        let mut scratch = zeno::Scratch::new();
        let budget = super::super::renderer::Budgets::new(Default::default()).transient;
        let mut cases = 0;
        for width in [1.171875, 3.125, 6.25] {
            for join in [zeno::Join::Round, zeno::Join::Miter, zeno::Join::Bevel] {
                for dashes in [&[][..], &[2., 3.][..]] {
                    let style = zeno::Style::Stroke(zeno::Stroke {
                        width,
                        join,
                        dashes,
                        offset: 0.25,
                        ..Default::default()
                    });
                    for outline in &outlines {
                        let expected = bitmap(outline.path(), style, &mut scratch);
                        let mut expanded = stroke::StrokePath::new(&budget, 1 << 20).unwrap();
                        let fill = scratch.apply(outline.path(), style, None, &mut expanded);
                        expanded.finish().unwrap();
                        let actual = bitmap(expanded.commands(), fill.into(), &mut scratch);
                        assert_eq!(
                            actual,
                            expected,
                            "width={width} commands={}",
                            expanded.commands().len()
                        );
                        cases += 1;
                    }
                }
            }
        }
        let style = zeno::Style::Stroke(zeno::Stroke {
            width: 3.125,
            join: zeno::Join::Round,
            ..Default::default()
        });
        let mut times = [Vec::new(), Vec::new()];
        for repetition in 0..20 {
            for mode in [repetition % 2, 1 - repetition % 2] {
                let began = Instant::now();
                for outline in &outlines {
                    let output = if mode == 0 {
                        bitmap(outline.path(), style, &mut scratch)
                    } else {
                        let mut expanded = stroke::StrokePath::new(&budget, 1 << 20).unwrap();
                        let fill = scratch.apply(outline.path(), style, None, &mut expanded);
                        expanded.finish().unwrap();
                        bitmap(expanded.commands(), fill.into(), &mut scratch)
                    };
                    black_box(output);
                }
                times[mode].push(began.elapsed().as_micros());
            }
        }
        times.iter_mut().for_each(|samples| samples.sort_unstable());
        assert_eq!(budget.usage().bytes, 0);
        eprintln!(
            "stroke masks: cases={cases}, glyphs={} baseline_p50_us={} expanded_p50_us={} scratch_peak_bytes={}",
            outlines.len(),
            times[0][10],
            times[1][10],
            budget.usage().peak_bytes
        );
    }
}
