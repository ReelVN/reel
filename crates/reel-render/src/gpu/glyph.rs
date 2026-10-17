//! Consumes shaped glyph IDs/positions; this module never invokes Swash shaping.
use super::GpuError;
use reel_ui::frame::{FrameGlyphRun, RecordLocation, RenderFrame};
use reel_ui::resources::ResourceData;
use reel_ui::{kurbo, peniko};
use swash::{
    scale::{ScaleContext, outline::Outline},
    zeno,
};

#[derive(Default)]
pub(super) struct Rasterizer {
    scale: ScaleContext,
    outline: Outline,
    scratch: zeno::Scratch,
}
pub struct GlyphBitmap {
    pub x: f64,
    pub y: f64,
    pub size: [u32; 2],
    pub alpha: Vec<u8>,
}

pub struct RasterLimits {
    pub available: u64,
    pub max_dimension: u32,
}

/// Power-of-two densities keep animated transforms on reusable atlas tiers.
pub(super) fn raster_density(transform: kurbo::Affine) -> f64 {
    let [a, b, c, d, _, _] = transform.as_coeffs();
    let magnification = ((a + d).hypot(b - c) + (a - d).hypot(b + c)) * 0.5;
    if magnification > 1. {
        2_f64.powf(magnification.log2().ceil())
    } else {
        1.
    }
}

pub fn rasterize(
    context: &mut Rasterizer,
    frame: &RenderFrame,
    run: &FrameGlyphRun,
    density: f64,
    index: usize,
    at: RecordLocation,
    RasterLimits {
        available,
        max_dimension,
    }: RasterLimits,
) -> Result<Option<GlyphBitmap>, GpuError> {
    let resource = frame
        .resources()
        .leases()
        .iter()
        .find(|r| r.handle() == run.font)
        .ok_or(GpuError::Unsupported {
            at,
            record: "missing font lease",
        })?;
    let ResourceData::Font(font) = resource.data() else {
        return Err(GpuError::Unsupported {
            at,
            record: "font resource type",
        });
    };
    let font = swash::FontRef::from_index(font.data(), run.font_index as usize).ok_or(
        GpuError::Unsupported {
            at,
            record: "font collection index",
        },
    )?;
    let glyph = &run.glyphs[index];
    let id = u16::try_from(glyph.id).map_err(|_| GpuError::Unsupported {
        at,
        record: "glyph id",
    })?;
    let mut scaler = context
        .scale
        .builder(font)
        .size(run.font_size * density as f32)
        .hint(run.hint)
        .normalized_coords(run.normalized_coords.iter().copied())
        .build();
    if scaler.scale_color_outline(id).is_some() {
        return Err(GpuError::Unsupported {
            at,
            record: "color glyph layers",
        });
    }
    let outline = &mut context.outline;
    if !scaler.scale_outline_into(id, outline) {
        return Err(GpuError::Unsupported {
            at,
            record: "missing glyph outline",
        });
    }
    if outline.points().is_empty() {
        return Ok(None);
    }
    if run.font_embolden != kurbo::Vec2::ZERO {
        outline.embolden(
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
    // Measure the final stroked outline before allocating its coverage buffer.
    // Reuse Swash's rasterizer, including its scratch storage, without shaping again.
    let mut mask = zeno::Mask::with_scratch(outline.path(), &mut context.scratch);
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
    let mut alpha = vec![0; size[0] as usize * size[1] as usize];
    let placement = mask.render_into(&mut alpha, None);
    Ok(Some(GlyphBitmap {
        x: f64::from(placement.left),
        y: -f64::from(placement.top),
        size,
        alpha,
    }))
}

fn cap(cap: kurbo::Cap) -> zeno::Cap {
    match cap {
        kurbo::Cap::Butt => zeno::Cap::Butt,
        kurbo::Cap::Square => zeno::Cap::Square,
        kurbo::Cap::Round => zeno::Cap::Round,
    }
}
