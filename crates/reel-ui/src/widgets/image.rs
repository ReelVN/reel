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
use crate::resources::{ResourceImageBrush, ResourceLease};

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
/// This currently uses bilinear interpolation, which falls down when the image is
/// larger than its layout size (e.g. it is in a constrained layout smaller
/// than the image size).
///
/// You can change the sizing of the image with the [`ObjectFit`] property.
pub struct Image {
    image_data: Option<ImageBrush>,
    resource: Option<ResourceLease>,
    tiles: Vec<ImageTile>,
    tiled_size: Size,
    slice: Option<Insets>,
    slice_scale: f64,
    slice_center: bool,
    decorative: bool,
    alt_text: Option<ArcStr>,
    color: Color,
    repeat: [bool; 2],
    tile_scale: f64,
    corner_colors: Option<[Color; 4]>,
}

/// One resident tile. The image includes its sampling gutter; `clip` is the
/// tile interior in original-image coordinates and `transform` maps its pixels.
#[derive(Clone, Debug)]
pub struct ImageTile {
    pub image: ImageBrush,
    pub transform: Affine,
    pub clip: Rect,
}

// --- MARK: BUILDERS
impl Image {
    /// Creates an image drawing widget from an image buffer.
    ///
    /// By default, the image will be scaled to fully fit within the container.
    /// ([`ObjectFit::Contain`]).
    #[inline]
    pub fn new(image_data: impl Into<ImageBrush>) -> Self {
        Self {
            image_data: Some(image_data.into()),
            resource: None,
            tiles: Vec::new(),
            tiled_size: Size::ZERO,
            slice: None,
            slice_scale: 1.,
            slice_center: true,
            decorative: false,
            alt_text: None,
            color: Color::WHITE,
            repeat: [false; 2],
            tile_scale: 1.,
            corner_colors: None,
        }
    }

    /// Metadata-only images have no tiles until their visible resources arrive.
    pub fn tiled(size: Size, tiles: Vec<ImageTile>) -> Self {
        Self {
            image_data: None,
            resource: None,
            tiles,
            tiled_size: size,
            slice: None,
            slice_scale: 1.,
            slice_center: true,
            decorative: false,
            alt_text: None,
            color: Color::WHITE,
            repeat: [false; 2],
            tile_scale: 1.,
            corner_colors: None,
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
        let [w, h] = lease.data().image_size().expect("image resource");
        let mut image = Self::tiled(Size::new(w.into(), h.into()), Vec::new());
        image.resource = Some(lease);
        image
    }

    /// Replacing a same-sized decoded frame invalidates paint, not layout.
    pub fn set_resource(this: &mut WidgetMut<'_, Self>, lease: ResourceLease) {
        let [w, h] = lease.data().image_size().expect("image resource");
        let size = Size::new(w.into(), h.into());
        let layout_changed = this.widget.preferred_size() != size;
        this.widget.image_data = None;
        this.widget.tiles.clear();
        this.widget.tiled_size = size;
        this.widget.resource = Some(lease);
        if layout_changed {
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
        this.widget.image_data = Some(image_data.into());
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
    /// Returns the preferred size of the image.
    ///
    /// The returned size is in logical pixels.
    ///
    /// This takes into account [`IMAGE_SCALE`], so a high-resolution resource can
    /// have a stable preferred logical size.
    fn preferred_size(&self) -> Size {
        let Some(image_data) = &self.image_data else {
            return self.tiled_size;
        };
        Size::new(
            image_data.image.width as f64 / IMAGE_SCALE,
            image_data.image.height as f64 / IMAGE_SCALE,
        )
    }

    fn paint_sliced(&self, painter: &mut Painter<'_>, bounds: Rect, insets: Insets) {
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
        let source_x = [0., left, width - right, width];
        let source_y = [0., top, height - bottom, height];
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
                if source.width() <= 0.
                    || source.height() <= 0.
                    || target.width() <= 0.
                    || target.height() <= 0.
                {
                    continue;
                }
                let scale_x = target.width() / source.width();
                let scale_y = target.height() / source.height();
                let transform = Affine::new([
                    scale_x,
                    0.,
                    0.,
                    scale_y,
                    target.x0 - source.x0 * scale_x,
                    target.y0 - source.y0 * scale_y,
                ]);
                // Each rectangle already lies in the content box. Shared brush
                // identity keeps the same resource; no per-slice clip layer.
                self.fill(painter, target, transform);
            }
        }
    }

    fn fill(&self, painter: &mut Painter<'_>, bounds: Rect, transform: Affine) {
        if let Some(image) = &self.image_data {
            let mut brush = image.clone();
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
            painter
                .fill(bounds, &brush)
                .brush_transform(Some(transform))
                .draw();
        } else {
            painter
                .fill(bounds, Color::WHITE)
                .brush_transform(Some(transform))
                .draw();
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

    fn layout(&mut self, _ctx: &mut LayoutCtx<'_>, _props: &PropertiesRef<'_>, _size: Size) {}

    fn paint(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        props: &PropertiesRef<'_>,
        painter: &mut Painter<'_>,
    ) {
        ctx.set_image_color(self.color);
        ctx.set_image_resource(self.resource.as_ref().map(|lease| ResourceImageBrush {
            resource: lease.handle(),
            sampler: crate::peniko::ImageSampler {
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
            },
        }));
        let content_box = ctx.content_box();
        ctx.set_image_corner_colors(self.corner_colors, content_box);
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
                let transform = Affine::translate(content_box.origin().to_vec2())
                    * Affine::scale(self.tile_scale);
                self.fill(painter, bounds, transform);
            }
            return;
        }
        if let Some(slice) = self.slice {
            self.paint_sliced(painter, content_box, slice);
            return;
        }
        let cache = ctx.property_cache();
        let object_fit = props.get::<ObjectFit>(cache);
        // For drawing we want to scale the actual image data lengths, which means
        // we need to avoid using Image::preferred_size which does not match the data.
        let image_size = self.preferred_size();
        let transform = object_fit.affine(content_box, image_size.to_rect());

        painter.with_fill_clip(content_box, |painter| {
            if let Some(image) = &self.image_data {
                painter.draw_image(image, transform);
            } else if self.resource.is_some() {
                painter
                    .fill(image_size.to_rect(), Color::WHITE)
                    .transform(transform)
                    .draw();
            } else {
                for tile in &self.tiles {
                    // Adjacent interiors share rasterization edges on the same
                    // target. Separate antialiased clip layers would blend their
                    // complementary coverage twice and leave seams under rotation.
                    painter
                        .fill(tile.clip, &tile.image)
                        .transform(transform)
                        .brush_transform(Some(tile.transform))
                        .draw();
                }
            }
        });
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

// FIXME - remove cfg?
// --- MARK: TESTS
