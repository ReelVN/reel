use super::*;
use reel_ui::{app::PresentedGeometryView, core::WidgetId, properties::ObjectFit};

pub(crate) struct ImageGeometry {
    pub source: String,
    pub widget: WidgetId,
    pub fit: ObjectFit,
    pub source_rect: Option<Rect>,
    /// Nearest effect source boundary and its complete logical window-space box.
    pub effect_domain: Option<(WidgetId, Rect)>,
}
#[derive(Clone, Default)]
pub(crate) struct ImageRegion {
    pub tiles: Vec<String>,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TileDemand {
    pub owner: String,
    pub required: bool,
    pub dimensions: [u32; 2],
    pub transform: Affine,
    pub clip: Rect,
}
impl TileDemand {
    pub fn bytes(&self) -> u64 {
        u64::from(self.dimensions[0]) * u64::from(self.dimensions[1]) * 4
    }
}

/// Conservative window-space clipping shared by raster and tiled image demand.
/// Rotated clips use their enclosing rectangle; uncertainty keeps the resource.
pub(crate) fn clipped_viewport(viewport: Rect, transform: Affine, clip: Option<Rect>) -> Rect {
    if let Some(clip) = clip {
        let bounds = transform.transform_rect_bbox(clip);
        if bounds.is_finite() {
            return viewport.intersect(bounds);
        }
    }
    viewport
}

/// Use exactly the widget geometry used by paint and accepted-frame input.
pub(crate) fn visible_tiles(
    manifest: PreparedImage,
    image: &ImageGeometry,
    geometry: &PresentedGeometryView,
) -> (ImageRegion, BTreeMap<String, TileDemand>) {
    let mut region = ImageRegion::default();
    let mut tiles = BTreeMap::new();
    let Some(record) = geometry.get(image.widget) else {
        return (region, tiles);
    };
    let root = geometry.get(geometry.root_id()).unwrap();
    let mut visible = image.effect_domain.map_or_else(
        || {
            root.window_transform
                .transform_rect_bbox(root.size.to_rect())
        },
        |(_, bounds)| bounds,
    );
    let mut ancestor = Some(record);
    while let Some(node) = ancestor {
        if node.is_stashed {
            return (region, tiles);
        }
        // The effect samples its complete logical source. Its own group clip
        // and all outer clips are applied after the fragment has run.
        if image
            .effect_domain
            .is_some_and(|(boundary, _)| boundary == node.id)
        {
            break;
        }
        visible = clipped_viewport(visible, node.window_transform, node.clip_path);
        ancestor = node.parent.map(|index| &geometry.records()[index]);
    }
    let image_size = Size::new(f64::from(manifest.width), f64::from(manifest.height));
    let source_rect = image.source_rect.unwrap_or(image_size.to_rect());
    let fit = image
        .fit
        .affine(record.size.to_rect(), source_rect.size().to_rect());
    let transform =
        record.window_transform * fit * Affine::translate(-source_rect.origin().to_vec2());
    let coefficients = transform.as_coeffs();
    let scale = coefficients[0]
        .hypot(coefficients[1])
        .max(coefficients[2].hypot(coefficients[3]))
        * geometry.scale_factor();
    if !scale.is_finite() || scale <= 0. || transform.determinant() == 0. {
        return (region, tiles);
    }
    visible = visible.intersect(
        record
            .window_transform
            .transform_rect_bbox(record.size.to_rect()),
    );
    let source_visible = transform
        .inverse()
        .transform_rect_bbox(visible)
        .intersect(source_rect);
    if source_visible.width() <= 0. || source_visible.height() <= 0. {
        return (region, tiles);
    }
    // Source windows outside the image pad its nearest edge, just like an
    // ordinary whole-image source_rect. Demand those edge texels as well.
    let sample_axis = |start: f64, end: f64, size: f64| {
        if end <= 0. {
            (0., 1.)
        } else if start >= size {
            (size - 1., size)
        } else {
            (start.max(0.), end.min(size))
        }
    };
    let (x0, x1) = sample_axis(source_visible.x0, source_visible.x1, image_size.width);
    let (y0, y1) = sample_axis(source_visible.y0, source_visible.y1, image_size.height);
    let source_visible = Rect::new(x0, y0, x1, y1);
    let level = ((1. / scale).log2().floor().max(0.) as u32)
        .clamp(manifest.first_level, manifest.levels() - 1);
    let [width, height] = manifest.dimensions(level);
    // Each level resizes the full image. Odd dimensions therefore have their own
    // exact x/y scale, rather than accumulating rounded power-of-two edge drift.
    let sx = f64::from(manifest.width) / f64::from(width);
    let sy = f64::from(manifest.height) / f64::from(height);
    let edge = f64::from(manifest.tile_edge);
    let nx = width.div_ceil(manifest.tile_edge);
    let ny = height.div_ceil(manifest.tile_edge);
    let x0 = (source_visible.x0 / sx / edge).floor().max(0.) as u32;
    let y0 = (source_visible.y0 / sy / edge).floor().max(0.) as u32;
    let x1 = ((source_visible.x1 / sx / edge).ceil() as u32).min(nx);
    let y1 = ((source_visible.y1 / sy / edge).ceil() as u32).min(ny);
    for y in y0.saturating_sub(1)..y1.saturating_add(1).min(ny) {
        for x in x0.saturating_sub(1)..x1.saturating_add(1).min(nx) {
            let required = x >= x0 && x < x1 && y >= y0 && y < y1;
            let dimensions = manifest.tile_dimensions(level, x, y).unwrap();
            let key = manifest.tile_source(&image.source, level, x, y);
            if required {
                region.tiles.push(key.clone());
            }
            let left = f64::from(x) * edge;
            let top = f64::from(y) * edge;
            tiles.insert(
                key,
                TileDemand {
                    owner: image.source.clone(),
                    required,
                    dimensions,
                    transform: Affine::scale_non_uniform(sx, sy)
                        * Affine::translate((left - 1., top - 1.)),
                    clip: Rect::new(
                        left * sx,
                        top * sy,
                        (left + f64::from(dimensions[0] - 2)) * sx,
                        (top + f64::from(dimensions[1] - 2)) * sy,
                    ),
                },
            );
        }
    }
    (region, tiles)
}
