//! Consumes shaped glyph IDs/positions; this module never invokes Swash shaping.
use super::GpuError;
use reel_ui::frame::{FrameGlyphRun, RecordLocation, RenderFrame};
use reel_ui::resources::ResourceData;
use swash::scale::{Render, ScaleContext, Source, image::Content};
pub struct GlyphBitmap {
    pub x: f64,
    pub y: f64,
    pub size: [u32; 2],
    pub pixels: Vec<f32>,
}
pub fn rasterize(
    frame: &RenderFrame,
    run: &FrameGlyphRun,
    index: usize,
    at: RecordLocation,
    available: u64,
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
    let font = swash::FontRef::from_index(font.data.data(), run.font_index as usize).ok_or(
        GpuError::Unsupported {
            at,
            record: "font collection index",
        },
    )?;
    if run.font_size > 256. || run.font_embolden != reel_ui::kurbo::Vec2::ZERO {
        return Err(GpuError::Unsupported {
            at,
            record: "glyph size above 256 or embolden",
        });
    }
    let glyph = &run.glyphs[index];
    let id = u16::try_from(glyph.id).map_err(|_| GpuError::Unsupported {
        at,
        record: "glyph id",
    })?;
    let mut context = ScaleContext::new();
    let mut scaler = context
        .builder(font)
        .size(run.font_size)
        .hint(run.hint)
        .normalized_coords(run.normalized_coords.iter().copied())
        .build();
    if scaler.scale_color_outline(id).is_some() {
        return Err(GpuError::Unsupported {
            at,
            record: "color glyph layers",
        });
    }
    if let Some(outline) = scaler.scale_outline(id) {
        let bounds = outline.bounds();
        let width = bounds.width().ceil() + 4.;
        let height = bounds.height().ceil() + 4.;
        if !width.is_finite() || !height.is_finite() || width > 4096. || height > 4096. {
            return Err(GpuError::Unsupported {
                at,
                record: "glyph raster extent",
            });
        }
        let cost = super::resources::upload_cost([width.max(1.) as u32, height.max(1.) as u32])?;
        if cost > available {
            return Err(GpuError::Budget {
                domain: "glyph raster",
                requested: cost,
                available,
            });
        }
    }
    let Some(image) = Render::new(&[Source::Outline])
        .format(swash::zeno::Format::Alpha)
        .render(&mut scaler, id)
    else {
        if scaler
            .scale_outline(id)
            .is_some_and(|o| o.points().is_empty())
        {
            return Ok(None);
        }
        return Err(GpuError::Unsupported {
            at,
            record: "missing glyph outline",
        });
    };
    let size = [image.placement.width, image.placement.height];
    if size.contains(&0) {
        return Ok(None);
    }
    let cost = super::resources::upload_cost(size)?;
    if cost > available {
        return Err(GpuError::Budget {
            domain: "glyph",
            requested: cost,
            available,
        });
    }
    let mut pixels = Vec::with_capacity(size[0] as usize * size[1] as usize * 4);
    match image.content {
        Content::Mask => {
            for a in image.data {
                pixels.extend([f32::from(a) / 255.; 4]);
            }
        }
        Content::Color => {
            return Err(GpuError::Unsupported {
                at,
                record: "color glyph encoding",
            });
        }
        Content::SubpixelMask => {
            return Err(GpuError::Unsupported {
                at,
                record: "subpixel glyph",
            });
        }
    }
    Ok(Some(GlyphBitmap {
        x: f64::from(glyph.x) + f64::from(image.placement.left),
        y: f64::from(glyph.y) - f64::from(image.placement.top),
        size,
        pixels,
    }))
}
