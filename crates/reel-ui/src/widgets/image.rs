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
use crate::kurbo::{Affine, Axis, Rect, Size};
use crate::layout::{LenReq, Length};
use crate::peniko::ImageBrush;
use crate::properties::ObjectFit;

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
    tiles: Vec<ImageTile>,
    tiled_size: Size,
    decorative: bool,
    alt_text: Option<ArcStr>,
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
            tiles: Vec::new(),
            tiled_size: Size::ZERO,
            decorative: false,
            alt_text: None,
        }
    }

    /// Metadata-only images have no tiles until their visible resources arrive.
    pub fn tiled(size: Size, tiles: Vec<ImageTile>) -> Self {
        Self {
            image_data: None,
            tiles,
            tiled_size: size,
            decorative: false,
            alt_text: None,
        }
    }

    pub fn set_tiles(this: &mut WidgetMut<'_, Self>, size: Size, tiles: Vec<ImageTile>) {
        let layout_changed = this.widget.image_data.is_some() || this.widget.tiled_size != size;
        this.widget.image_data = None;
        this.widget.tiles = tiles;
        this.widget.tiled_size = size;
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
    /// Sets new `ImageBrush`.
    #[inline]
    pub fn set_image_data(this: &mut WidgetMut<'_, Self>, image_data: impl Into<ImageBrush>) {
        this.widget.image_data = Some(image_data.into());
        this.widget.tiles.clear();
        this.ctx.request_layout();
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
        let object_fit = props.get::<ObjectFit>(cache);
        let preferred_size = self.preferred_size();

        object_fit.measure(axis, len_req, cross_length, preferred_size)
    }

    fn layout(&mut self, _ctx: &mut LayoutCtx<'_>, _props: &PropertiesRef<'_>, _size: Size) {}

    fn paint(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        props: &PropertiesRef<'_>,
        painter: &mut Painter<'_>,
    ) {
        let content_box = ctx.content_box();
        let cache = ctx.property_cache();
        let object_fit = props.get::<ObjectFit>(cache);
        // For drawing we want to scale the actual image data lengths, which means
        // we need to avoid using Image::preferred_size which does not match the data.
        let image_size = self.preferred_size();
        let transform = object_fit.affine(content_box, image_size.to_rect());

        painter.with_fill_clip(content_box, |painter| {
            if let Some(image) = &self.image_data {
                painter.draw_image(image, transform);
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

    fn make_trace_span(&self, id: WidgetId) -> Span {
        trace_span!("Image", id = id.trace())
    }
}

// FIXME - remove cfg?
// --- MARK: TESTS
