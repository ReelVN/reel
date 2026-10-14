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
    logical_size: Option<Size>,
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
            logical_size: Some(Size::new(f64::from(size[0]), f64::from(size[1]))),
            viewport: Size::ZERO,
        }
    }
    pub fn viewport(child: NewWidget<impl Widget + ?Sized>) -> Self {
        Self {
            child: child.erased().to_pod(),
            logical_size: None,
            viewport: Size::ZERO,
        }
    }
    pub fn set_size(this: &mut WidgetMut<'_, Self>, size: Option<[u32; 2]>) {
        let size = size.map(|[w, h]| {
            assert!(w > 0 && h > 0, "canvas dimensions must be positive");
            Size::new(f64::from(w), f64::from(h))
        });
        if this.widget.logical_size != size {
            this.widget.logical_size = size;
            this.ctx.request_layout();
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
                self.logical_size.unwrap_or(self.viewport).width
            } else {
                self.logical_size.unwrap_or(self.viewport).height
            }),
        }
    }
    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, size: Size) {
        self.viewport = size;
        ctx.set_clip_path(size.to_rect());
        ctx.run_layout(&mut self.child, self.logical_size.unwrap_or(size));
        ctx.place_child(&mut self.child, Point::ORIGIN);
    }
    fn compose(&mut self, ctx: &mut ComposeCtx<'_>) {
        let (scale, x, y) = if let Some(logical) = self.logical_size {
            let scale =
                (self.viewport.width / logical.width).min(self.viewport.height / logical.height);
            (
                scale,
                (self.viewport.width - logical.width * scale) * 0.5,
                (self.viewport.height - logical.height * scale) * 0.5,
            )
        } else {
            (1., 0., 0.)
        };
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
