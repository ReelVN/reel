//! A fixed logical canvas fitted into its available viewport.
use crate::{
    core::*,
    imaging::Painter,
    kurbo::{Affine, Axis, Point, Size},
    layout::{LenReq, Length},
};
use accesskit::{Node, Role};
use std::any::TypeId;
use tracing::{Span, trace_span};

pub struct Canvas {
    child: WidgetPod<dyn Widget>,
    logical_size: Size,
    viewport: Size,
}
impl Canvas {
    pub fn new(child: NewWidget<impl Widget + ?Sized>, size: [u32; 2]) -> Self {
        assert!(
            size[0] > 0 && size[1] > 0,
            "canvas dimensions must be positive"
        );
        Self {
            child: child.erased().to_pod(),
            logical_size: Size::new(f64::from(size[0]), f64::from(size[1])),
            viewport: Size::ZERO,
        }
    }
}
impl Widget for Canvas {
    type Action = NoAction;
    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        ctx.register_child(&mut self.child);
    }
    fn property_changed(&mut self, _: &mut UpdateCtx<'_>, _: TypeId) {}
    fn measure(
        &mut self,
        _: &mut MeasureCtx<'_>,
        _: &PropertiesRef<'_>,
        axis: Axis,
        request: LenReq,
        _: Option<Length>,
    ) -> Length {
        match request {
            LenReq::FitContent(space) => space,
            _ => Length::px(if axis == Axis::Horizontal {
                self.logical_size.width
            } else {
                self.logical_size.height
            }),
        }
    }
    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, size: Size) {
        self.viewport = size;
        ctx.set_clip_path(size.to_rect());
        ctx.run_layout(&mut self.child, self.logical_size);
        ctx.place_child(&mut self.child, Point::ORIGIN);
    }
    fn compose(&mut self, ctx: &mut ComposeCtx<'_>) {
        let scale = (self.viewport.width / self.logical_size.width)
            .min(self.viewport.height / self.logical_size.height);
        let x = (self.viewport.width - self.logical_size.width * scale) * 0.5;
        let y = (self.viewport.height - self.logical_size.height * scale) * 0.5;
        ctx.set_child_transform(
            &mut self.child,
            Affine::translate((x, y)) * Affine::scale(scale),
        );
    }
    fn paint(&mut self, _: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, _: &mut Painter<'_>) {}
    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }
    fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, _: &mut Node) {}
    fn children_ids(&self) -> ChildrenIds {
        [self.child.id()].into_iter().collect()
    }
    fn make_trace_span(&self, id: WidgetId) -> Span {
        trace_span!("Canvas", id = id.trace())
    }
}
