// Copyright 2020 the Xilem Authors and the Druid Authors
// SPDX-License-Identifier: Apache-2.0

use std::any::TypeId;

use accesskit::{Node, Role};
use tracing::{Span, trace_span};

use crate::core::{
    AccessCtx, ArcStr, ChildrenIds, LayoutCtx, MeasureCtx, NoAction, PaintCtx, PropertiesMut,
    PropertiesRef, Property, RegisterCtx, Update, UpdateCtx, UsesProperty, Widget, WidgetId,
    WidgetMut,
};
use crate::imaging::Painter;
use crate::kurbo::{Affine, Axis, Insets, Rect, Size};
use crate::layout::{LenReq, Length};
use crate::peniko::{Color, Extend, ImageBrush};
use crate::properties::ObjectFit;
use crate::resources::ResourceLease;

// TODO: Make this a configurable option of the widget.
/// The scale that the image is native to.
///
/// Used to calculate the preferred logical size of the image.
///
/// For example you might have a 256 px icon meant for crisp details on 8K screens.
/// Then the image could have an preferred scale of e.g. 4.0,
/// meaning that its preferred logical size is 64px.
/// That way the image looks good at any scale and doesn't shift the layout around.
const IMAGE_SCALE: f64 = 1.0;

/// A widget that renders a bitmap Image.
///
/// The underlying image uses `Arc` for buffer data, making it cheap to clone.
///
/// Filtering defaults to bilinear interpolation. Nearest filtering preserves
/// pixel edges while sharing the same image storage.
///
/// You can change the sizing of the image with the [`ObjectFit`] property.
pub struct Image {
    image_data: Option<ImageBrush>,
    resource: Option<ResourceLease>,
    tiles: Vec<ImageTile>,
    tiled_size: Size,
    source_rect: Option<Rect>,
    sampling: crate::peniko::ImageQuality,
    alpha_layout: crate::frame::ImageAlphaLayout,
    slice: Option<Insets>,
    slice_scale: f64,
    slice_center: bool,
    decorative: bool,
    alt_text: Option<ArcStr>,
    color: Color,
    repeat: [bool; 2],
    tile_scale: f64,
    corner_colors: Option<[Color; 4]>,
    adjustment: crate::frame::ImageColorAdjustment,
    content_size: Size,
    paint_visible: bool,
}

/// One resident tile, including its sampling gutter. `clip` is its interior in
/// original-image coordinates; `transform` maps logical tile pixels into that image.
#[derive(Clone, Debug)]
pub struct ImageTile {
    /// Actual raster or prepared-texture storage retained by this tile draw.
    pub resource: ResourceLease,
    /// Forward mapping from logical tile pixels into original-image coordinates.
    pub transform: Affine,
    /// Tile interior in original-image coordinates, excluding its gutter.
    pub clip: Rect,
}

// --- MARK: BUILDERS
impl Image {
    /// Select per-draw filtering without copying or replacing image storage.
    pub fn with_sampling(mut self, sampling: crate::peniko::ImageQuality) -> Self {
        self.sampling = sampling;
        if let Some(image) = &mut self.image_data {
            image.sampler.quality = sampling;
        }
        self
    }

    /// Creates an image drawing widget from an image buffer.
    ///
    /// By default, the image will be scaled to fully fit within the container.
    /// ([`ObjectFit::Contain`]).
    #[inline]
    pub fn new(image_data: impl Into<ImageBrush>) -> Self {
        let image_data = image_data.into();
        let sampling = image_data.sampler.quality;
        Self {
            image_data: Some(image_data),
            resource: None,
            tiles: Vec::new(),
            tiled_size: Size::ZERO,
            source_rect: None,
            sampling,
            alpha_layout: Default::default(),
            slice: None,
            slice_scale: 1.,
            slice_center: true,
            decorative: false,
            alt_text: None,
            color: Color::WHITE,
            repeat: [false; 2],
            tile_scale: 1.,
            corner_colors: None,
            adjustment: Default::default(),
            content_size: Size::ZERO,
            paint_visible: true,
        }
    }

    /// Metadata-only images have no tiles until their visible resources arrive.
    pub fn tiled(size: Size, tiles: Vec<ImageTile>) -> Self {
        Self {
            image_data: None,
            resource: None,
            tiles,
            tiled_size: size,
            source_rect: None,
            sampling: Default::default(),
            alpha_layout: Default::default(),
            slice: None,
            slice_scale: 1.,
            slice_center: true,
            decorative: false,
            alt_text: None,
            color: Color::WHITE,
            repeat: [false; 2],
            tile_scale: 1.,
            corner_colors: None,
            adjustment: Default::default(),
            content_size: Size::ZERO,
            paint_visible: true,
        }
    }

    pub fn set_tiles(this: &mut WidgetMut<'_, Self>, size: Size, tiles: Vec<ImageTile>) {
        let layout_changed = this.widget.image_data.is_some() || this.widget.tiled_size != size;
        this.widget.image_data = None;
        this.widget.resource = None;
        this.widget.tiles = tiles;
        this.widget.tiled_size = size;
        if layout_changed {
            this.ctx.request_layout();
        } else {
            this.ctx.request_render();
        }
    }

    /// Draw a decoded host resource without manufacturing an RGBA image.
    pub fn from_resource(lease: ResourceLease) -> Self {
        let [w, h] = lease.image_size().expect("image resource");
        let mut image = Self::tiled(Size::new(w.into(), h.into()), Vec::new());
        image.resource = Some(lease);
        image
    }

    /// Replacing a same-sized decoded frame invalidates paint, not layout.
    pub fn set_resource(this: &mut WidgetMut<'_, Self>, lease: ResourceLease) {
        let [w, h] = lease.image_size().expect("image resource");
        let size = Size::new(w.into(), h.into());
        let old_size = this.widget.preferred_size();
        this.widget.image_data = None;
        this.widget.tiles.clear();
        this.widget.tiled_size = size;
        this.widget.resource = Some(lease);
        if this.widget.preferred_size() != old_size {
            this.ctx.request_layout();
        } else {
            this.ctx.request_render();
        }
    }

    /// Specifies whether the image is decorative, meaning it doesn't have meaningful content
    /// and is only for visual presentation.
    ///
    /// If `is_decorative` is `true`, the image will be ignored by screen readers.
    pub fn decorative(mut self, is_decorative: bool) -> Self {
        self.decorative = is_decorative;
        self
    }

    /// Multiply pixels by this color without changing the shared image resource.
    pub fn with_color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    /// Adjust unpremultiplied sRGB after ordinary and corner tint multiplication.
    /// The adjustment uses the shared source texture and preserves alpha.
    /// Values must satisfy [`crate::frame::ImageColorAdjustment::is_valid`].
    pub fn with_color_adjustment(mut self, adjustment: crate::frame::ImageColorAdjustment) -> Self {
        self.adjustment = adjustment;
        self
    }

    /// Select source pixels without changing the shared image resource.
    /// None uses the whole source. Finite endpoints and positive dimensions are
    /// required; out-of-texture coordinates pad the texture's edges.
    pub fn with_source_rect(mut self, rect: Option<Rect>) -> Self {
        self.source_rect = rect;
        self
    }

    /// Interpret packed alpha while sharing the original image or video resource.
    /// TopBottom halves the natural height; source windows use visible color pixels.
    pub fn with_alpha_layout(mut self, layout: crate::frame::ImageAlphaLayout) -> Self {
        self.alpha_layout = layout;
        self
    }

    /// Stretch a whole image as nine slices, preserving corner proportions.
    /// Insets are source pixels; scale maps border pixels to logical pixels.
    /// Both must be finite and nonnegative. `None` restores ordinary object fit.
    pub fn with_slice(mut self, slice: Option<Insets>, scale: f64, center: bool) -> Self {
        self.slice = slice;
        self.slice_scale = scale;
        self.slice_center = center;
        self
    }

    /// Repeat whole source pixels at a positive logical scale. Corners tint the
    /// whole content box in top-left, top-right, bottom-right, bottom-left order.
    pub fn with_pattern(
        mut self,
        repeat: [bool; 2],
        scale: f64,
        corners: Option<[Color; 4]>,
    ) -> Self {
        self.repeat = repeat;
        self.tile_scale = scale;
        self.corner_colors = corners;
        self
    }

    /// Sets the text that will describe the image to screen readers.
    ///
    /// Users are encouraged to set alt text for the image.
    /// If possible, the alt-text should succinctly describe what the image represents.
    ///
    /// If the image is decorative users should set alt text to `""`.
    /// If it's too hard to describe through text, the alt text should be left unset.
    /// This allows accessibility clients to know that there is no accessible description of the image content.
    pub fn with_alt_text(mut self, alt_text: impl Into<ArcStr>) -> Self {
        self.alt_text = Some(alt_text.into());
        self
    }
}

// --- MARK: WIDGETMUT
impl Image {
    /// Change filtering while preserving layout, resource identity and input owners.
    pub fn set_sampling(this: &mut WidgetMut<'_, Self>, sampling: crate::peniko::ImageQuality) {
        if this.widget.sampling != sampling {
            this.widget.sampling = sampling;
            if let Some(image) = &mut this.widget.image_data {
                image.sampler.quality = sampling;
            }
            this.ctx.request_render();
        }
    }

    /// Change packed alpha interpretation and update the natural image dimensions.
    pub fn set_alpha_layout(
        this: &mut WidgetMut<'_, Self>,
        layout: crate::frame::ImageAlphaLayout,
    ) {
        if this.widget.alpha_layout != layout {
            this.widget.alpha_layout = layout;
            this.ctx.request_layout();
        }
    }

    /// Change the source window. Moving a same-sized window invalidates only paint.
    pub fn set_source_rect(this: &mut WidgetMut<'_, Self>, rect: Option<Rect>) {
        if this.widget.source_rect != rect {
            let old_size = this.widget.preferred_size();
            this.widget.source_rect = rect;
            let new_size = this.widget.preferred_size();
            // Endpoint subtraction may differ by a float64 rounding bit when
            // only the origin moved. Layout follows drawing-size precision.
            if [old_size.width as f32, old_size.height as f32]
                != [new_size.width as f32, new_size.height as f32]
            {
                this.ctx.request_layout();
            } else {
                this.ctx.request_render();
            }
        }
    }

    /// Change image color adjustment, invalidating paint without changing layout.
    pub fn set_color_adjustment(
        this: &mut WidgetMut<'_, Self>,
        adjustment: crate::frame::ImageColorAdjustment,
    ) {
        if this.widget.adjustment != adjustment {
            this.widget.adjustment = adjustment;
            this.ctx.request_render();
        }
    }

    pub fn set_pattern(
        this: &mut WidgetMut<'_, Self>,
        repeat: [bool; 2],
        scale: f64,
        corners: Option<[Color; 4]>,
    ) {
        if this.widget.repeat != repeat
            || this.widget.tile_scale != scale
            || this.widget.corner_colors != corners
        {
            let layout_changed = this.widget.repeat != repeat
                || (repeat != [false; 2] && this.widget.tile_scale != scale);
            this.widget.repeat = repeat;
            this.widget.tile_scale = scale;
            this.widget.corner_colors = corners;
            if layout_changed {
                this.ctx.request_layout();
            } else {
                this.ctx.request_render();
            }
        }
    }

    /// Change nine-slice geometry without decoding or copying image pixels.
    pub fn set_slice(
        this: &mut WidgetMut<'_, Self>,
        slice: Option<Insets>,
        scale: f64,
        center: bool,
    ) {
        if this.widget.slice != slice
            || this.widget.slice_scale != scale
            || this.widget.slice_center != center
        {
            let layout_changed = this.widget.slice.is_some() != slice.is_some();
            this.widget.slice = slice;
            this.widget.slice_scale = scale;
            this.widget.slice_center = center;
            if layout_changed {
                this.ctx.request_layout();
            } else {
                this.ctx.request_render();
            }
        }
    }

    /// Update pixel multiplication without decoding, uploading or relaying out the image.
    pub fn set_color(this: &mut WidgetMut<'_, Self>, color: Color) {
        if this.widget.color != color {
            this.widget.color = color;
            this.ctx.request_render();
        }
    }

    /// Sets new `ImageBrush`.
    #[inline]
    pub fn set_image_data(this: &mut WidgetMut<'_, Self>, image_data: impl Into<ImageBrush>) {
        let old_size = this.widget.preferred_size();
        let image_data = image_data.into();
        this.widget.sampling = image_data.sampler.quality;
        this.widget.image_data = Some(image_data);
        this.widget.resource = None;
        this.widget.tiles.clear();
        if this.widget.preferred_size() != old_size {
            this.ctx.request_layout();
        } else {
            this.ctx.request_render();
        }
    }

    /// Sets whether the image is decorative, meaning it doesn't have meaningful content
    /// and is only for visual presentation.
    ///
    /// See [`Image::decorative`] for details.
    pub fn set_decorative(this: &mut WidgetMut<'_, Self>, is_decorative: bool) {
        this.widget.decorative = is_decorative;
        this.ctx.request_accessibility_update();
    }

    /// Sets the text that will describe the image to screen readers.
    ///
    /// See [`Image::with_alt_text`] for details.
    pub fn set_alt_text(this: &mut WidgetMut<'_, Self>, alt_text: Option<impl Into<ArcStr>>) {
        this.widget.alt_text = alt_text.map(Into::into);
        this.ctx.request_accessibility_update();
    }
}

impl Image {
    fn color_rect(&self) -> Option<Rect> {
        self.source_rect.or_else(|| {
            (self.alpha_layout == crate::frame::ImageAlphaLayout::TopBottom).then(|| {
                let size = self.image_data.as_ref().map_or(self.tiled_size, |image| {
                    Size::new(image.image.width.into(), image.image.height.into())
                });
                self.alpha_layout.size(size).to_rect()
            })
        })
    }

    /// Returns the preferred size of the image.
    ///
    /// The returned size is in logical pixels.
    ///
    /// This takes into account [`IMAGE_SCALE`], so a high-resolution resource can
    /// have a stable preferred logical size.
    fn preferred_size(&self) -> Size {
        if let Some(rect) = self.color_rect() {
            return rect.size() / IMAGE_SCALE;
        }
        let Some(image_data) = &self.image_data else {
            return self.tiled_size;
        };
        Size::new(
            image_data.image.width as f64 / IMAGE_SCALE,
            image_data.image.height as f64 / IMAGE_SCALE,
        )
    }

    fn paint_sliced(
        &self,
        ctx: &mut PaintCtx<'_>,
        painter: &mut Painter<'_>,
        bounds: Rect,
        insets: Insets,
    ) {
        if self.image_data.is_none() && self.resource.is_none() {
            return;
        }
        let Size { width, height } = self.preferred_size();
        let fit_borders = |width: f64, height: f64, horizontal: f64, vertical: f64| {
            let mut scale: f64 = 1.;
            if horizontal > 0. {
                scale = scale.min(width / horizontal);
            }
            if vertical > 0. {
                scale = scale.min(height / vertical);
            }
            scale
        };
        let source_scale = fit_borders(width, height, insets.x0 + insets.x1, insets.y0 + insets.y1);
        let left = insets.x0 * source_scale;
        let top = insets.y0 * source_scale;
        let right = insets.x1 * source_scale;
        let bottom = insets.y1 * source_scale;
        let target_scale = self.slice_scale
            * fit_borders(
                bounds.width(),
                bounds.height(),
                (left + right) * self.slice_scale,
                (top + bottom) * self.slice_scale,
            );
        let origin = self
            .color_rect()
            .map_or(crate::kurbo::Point::ZERO, |rect| rect.origin());
        let source_x = [
            origin.x,
            origin.x + left,
            origin.x + width - right,
            origin.x + width,
        ];
        let source_y = [
            origin.y,
            origin.y + top,
            origin.y + height - bottom,
            origin.y + height,
        ];
        let target_x = [
            bounds.x0,
            bounds.x0 + left * target_scale,
            bounds.x1 - right * target_scale,
            bounds.x1,
        ];
        let target_y = [
            bounds.y0,
            bounds.y0 + top * target_scale,
            bounds.y1 - bottom * target_scale,
            bounds.y1,
        ];
        for y in 0..3 {
            for x in 0..3 {
                if x == 1 && y == 1 && !self.slice_center {
                    continue;
                }
                let source = Rect::new(source_x[x], source_y[y], source_x[x + 1], source_y[y + 1]);
                let target = Rect::new(target_x[x], target_y[y], target_x[x + 1], target_y[y + 1]);
                if target.width() <= 0. || target.height() <= 0. {
                    continue;
                }
                // Destination-to-source sampling also represents a collapsed
                // source center: its corresponding coordinate stays constant.
                let scale_x = source.width() / target.width();
                let scale_y = source.height() / target.height();
                let transform = Affine::new([
                    scale_x,
                    0.,
                    0.,
                    scale_y,
                    source.x0 - target.x0 * scale_x,
                    source.y0 - target.y0 * scale_y,
                ]);
                // Each rectangle already lies in the content box. Shared brush
                // identity keeps the same resource; no per-slice clip layer.
                self.fill(ctx, painter, target, transform);
            }
        }
    }

    fn sampler(&self) -> crate::peniko::ImageSampler {
        crate::peniko::ImageSampler {
            quality: self.sampling,
            x_extend: if self.repeat[0] {
                Extend::Repeat
            } else {
                Extend::Pad
            },
            y_extend: if self.repeat[1] {
                Extend::Repeat
            } else {
                Extend::Pad
            },
            ..Default::default()
        }
    }
    fn fill(
        &self,
        ctx: &mut PaintCtx<'_>,
        painter: &mut Painter<'_>,
        bounds: Rect,
        transform: Affine,
    ) {
        if let Some(image) = &self.image_data {
            let mut brush = image.clone();
            if self.repeat != [false; 2] {
                brush.sampler.x_extend = if self.repeat[0] {
                    Extend::Repeat
                } else {
                    Extend::Pad
                };
                brush.sampler.y_extend = if self.repeat[1] {
                    Extend::Repeat
                } else {
                    Extend::Pad
                };
            }
            painter
                .fill(bounds, &brush)
                .brush_transform(Some(transform))
                .draw();
        } else if let Some(resource) = &self.resource {
            let sampling = if self.slice.is_some() || self.color_rect().is_some() {
                transform
            } else {
                transform.inverse()
            };
            ctx.draw_resource_image(
                painter,
                resource,
                bounds,
                Affine::IDENTITY,
                sampling,
                self.sampler(),
            );
        }
    }
}

impl UsesProperty<ObjectFit> for Image {}

// --- MARK: IMPL WIDGET
impl Widget for Image {
    type Action = NoAction;

    fn register_children(&mut self, _ctx: &mut RegisterCtx<'_>) {}

    fn property_changed(&mut self, ctx: &mut UpdateCtx<'_>, property_type: TypeId) {
        if ObjectFit::matches(property_type) {
            ctx.request_layout();
        }
    }

    fn update(
        &mut self,
        _ctx: &mut UpdateCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        _event: &Update,
    ) {
    }

    fn measure(
        &mut self,
        ctx: &mut MeasureCtx<'_>,
        props: &PropertiesRef<'_>,
        axis: Axis,
        len_req: LenReq,
        cross_length: Option<Length>,
    ) -> Length {
        let cache = ctx.property_cache();
        let object_fit = if self.slice.is_some() || self.repeat != [false; 2] {
            &ObjectFit::Stretch
        } else {
            props.get::<ObjectFit>(cache)
        };
        let preferred_size = self.preferred_size()
            * if self.repeat == [false; 2] {
                1.
            } else {
                self.tile_scale
            };

        object_fit.measure(axis, len_req, cross_length, preferred_size)
    }

    fn layout(&mut self, _ctx: &mut LayoutCtx<'_>, _props: &PropertiesRef<'_>, size: Size) {
        self.content_size = size;
    }

    fn set_paint_viewport(&mut self, viewport: Rect, content_to_window: Affine) -> bool {
        // Every image mode clips its own content to this box. Keep a pixel of
        // edge coverage and leave singular transforms on the complete path.
        let visible = content_to_window.determinant() == 0.
            || content_to_window
                .transform_rect_bbox(self.content_size.to_rect())
                .inflate(1., 1.)
                .overlaps(viewport);
        let changed = visible != self.paint_visible;
        self.paint_visible = visible;
        changed
    }

    fn paint(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        props: &PropertiesRef<'_>,
        painter: &mut Painter<'_>,
    ) {
        if !self.paint_visible {
            return;
        }
        ctx.set_image_color(self.color);
        let source_rect = self.color_rect();
        ctx.set_image_sampling_transform(self.slice.is_some() || source_rect.is_some());
        ctx.set_image_source_rect(source_rect);
        let content_box = ctx.content_box();
        ctx.set_image_appearance(
            self.corner_colors,
            self.adjustment,
            self.alpha_layout,
            content_box,
        );
        if self.repeat != [false; 2] {
            if self.image_data.is_some() || self.resource.is_some() {
                let image_size = self.preferred_size();
                let mut bounds = content_box;
                if !self.repeat[0] {
                    bounds.x1 = bounds
                        .x1
                        .min(bounds.x0 + image_size.width * self.tile_scale);
                }
                if !self.repeat[1] {
                    bounds.y1 = bounds
                        .y1
                        .min(bounds.y0 + image_size.height * self.tile_scale);
                }
                let transform = if let Some(rect) = source_rect {
                    Affine::translate(rect.origin().to_vec2())
                        * Affine::scale(1. / self.tile_scale)
                        * Affine::translate(-content_box.origin().to_vec2())
                } else {
                    Affine::translate(content_box.origin().to_vec2())
                        * Affine::scale(self.tile_scale)
                };
                self.fill(ctx, painter, bounds, transform);
            }
            return;
        }
        if let Some(slice) = self.slice {
            self.paint_sliced(ctx, painter, content_box, slice);
            return;
        }
        let cache = ctx.property_cache();
        let object_fit = props.get::<ObjectFit>(cache);
        // For drawing we want to scale the actual image data lengths, which means
        // we need to avoid using Image::preferred_size which does not match the data.
        let image_size = self.preferred_size();
        let transform = object_fit.affine(content_box, image_size.to_rect());

        if self.image_data.is_none() && self.resource.is_none() {
            let source = source_rect.unwrap_or(self.tiled_size.to_rect());
            let transform = transform * Affine::translate(-source.origin().to_vec2());
            if transform.determinant() == 0. {
                return;
            }
            let visible = transform
                .inverse()
                .transform_rect_bbox(content_box)
                .intersect(source);
            painter.with_fill_clip(content_box, |painter| {
                for tile in &self.tiles {
                    // Edge tiles also cover source windows extending beyond the
                    // image. Their clamped gutters preserve ordinary edge padding.
                    let mut interior = tile.clip;
                    if interior.x0 == 0. {
                        interior.x0 = interior.x0.min(source.x0);
                    }
                    if interior.y0 == 0. {
                        interior.y0 = interior.y0.min(source.y0);
                    }
                    if interior.x1.round() == self.tiled_size.width {
                        interior.x1 = interior.x1.max(source.x1);
                    }
                    if interior.y1.round() == self.tiled_size.height {
                        interior.y1 = interior.y1.max(source.y1);
                    }
                    let bounds = interior.intersect(visible);
                    if bounds.width() <= 0. || bounds.height() <= 0. {
                        continue;
                    }
                    let sampling = tile.transform.inverse();
                    ctx.set_image_source_rect(
                        source_rect.map(|rect| sampling.transform_rect_bbox(rect)),
                    );
                    // Crop adjacent tile interiors directly. Separate antialiased
                    // clip layers would double-blend their shared raster edges.
                    ctx.draw_resource_image(
                        painter,
                        &tile.resource,
                        bounds,
                        transform,
                        sampling,
                        self.sampler(),
                    );
                }
            });
            ctx.set_image_source_rect(source_rect);
            return;
        }

        // Object-fit is axis-aligned before the widget's transform. Crop the
        // quad in that local space so rotation and opacity need no clip target.
        if transform.determinant() == 0. {
            return;
        }
        let bounds = transform
            .inverse()
            .transform_rect_bbox(content_box)
            .intersect(image_size.to_rect());
        if bounds.width() <= 0. || bounds.height() <= 0. {
            return;
        }
        let sampling = source_rect.map_or(Affine::IDENTITY, |rect| {
            Affine::translate(rect.origin().to_vec2())
        });
        if let Some(image) = &self.image_data {
            painter
                .fill(bounds, image)
                .transform(transform)
                .brush_transform(Some(sampling))
                .draw();
        } else if let Some(resource) = &self.resource {
            ctx.draw_resource_image(
                painter,
                resource,
                bounds,
                transform,
                sampling,
                self.sampler(),
            );
        }
    }

    fn accessibility_role(&self) -> Role {
        Role::Image
    }

    fn accessibility(
        &mut self,
        _ctx: &mut AccessCtx<'_>,
        _props: &PropertiesRef<'_>,
        node: &mut Node,
    ) {
        if let Some(alt_text) = &self.alt_text {
            node.set_description(&**alt_text);
        }
        if self.decorative {
            node.set_hidden();
        }
    }

    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::new()
    }

    fn accepts_pointer_interaction(&self) -> bool {
        // Images are presentation. A skin layered above a slider must not
        // replace its control as the pointer target.
        false
    }

    fn make_trace_span(&self, id: WidgetId) -> Span {
        trace_span!("Image", id = id.trace())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{RenderRoot, RenderRootOptions, VisualLayerKind, VisualLayerPlan, WindowSizePolicy},
        core::{DefaultProperties, NewWidget},
        dpi::PhysicalSize,
        imaging::record::{Command, Draw},
        peniko::{Blob, Brush, ImageAlphaType, ImageData, ImageFormat},
    };
    use std::sync::Arc;

    fn image_draws(plan: &VisualLayerPlan) -> usize {
        plan.layers.iter().map(|layer| {
            let VisualLayerKind::Scene(scene) = &layer.kind else { return 0 };
            scene.commands().iter().filter(|command| {
                matches!(command, Command::Draw(id) if matches!(scene.draw_op(*id), Draw::Fill { brush: Brush::Image(_), .. }))
            }).count()
        }).sum()
    }

    #[test]
    fn images_leave_and_reenter_the_paint_viewport_without_replacing_pixels() {
        let image = ImageData {
            data: Blob::from(vec![255; 16]),
            width: 2,
            height: 2,
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
        };
        for (slice, repeat) in [
            (None, [false; 2]),
            (Some(Insets::uniform(0.5)), [false; 2]),
            (None, [true; 2]),
        ] {
            let widget = NewWidget::new(
                Image::new(image.clone())
                    .with_slice(slice, 1., true)
                    .with_pattern(repeat, 1., None),
            );
            let id = widget.id();
            let mut root = RenderRoot::new(
                widget,
                |_| {},
                RenderRootOptions {
                    default_properties: Arc::new(DefaultProperties::new()),
                    use_system_fonts: false,
                    size_policy: WindowSizePolicy::User,
                    size: PhysicalSize::new(200, 100),
                    scale_factor: 1.,
                    default_font: None,
                },
            );
            let before = image_draws(&root.redraw().0);
            assert!(before > 0);
            root.edit_widget(id, |mut widget| {
                widget.set_transform(Affine::translate((1000., 0.)))
            });
            assert_eq!(image_draws(&root.redraw().0), 0);
            root.edit_widget(id, |mut widget| {
                widget.set_transform(Affine::translate((0., -1000.)))
            });
            assert_eq!(image_draws(&root.redraw().0), 0);
            root.edit_widget(id, |mut widget| widget.set_transform(Affine::IDENTITY));
            assert_eq!(image_draws(&root.redraw().0), before);
            let widget = root.get_widget(id).unwrap().downcast::<Image>().unwrap();
            assert_eq!(
                widget.image_data.as_ref().unwrap().image.data.id(),
                image.data.id()
            );
        }
    }
}
