//! Typed child escape-hatch and animation checks.
use super::fixture::{StageGroup, Trace};
use masonry_core::{
    accesskit::{Node, Role},
    core::*,
    imaging::Painter,
    kurbo::{Affine, Axis, Insets, Point, Rect, Size},
    layout::{LenReq, Length},
};
use std::{cell::RefCell, rc::Rc};
impl AllowRawMut for StageGroup {}
pub type Samples = Rc<RefCell<Vec<(&'static str, Option<Affine>)>>>;
pub type OwnGeometry = (
    Rect,
    Rect,
    Rect,
    Rect,
    f64,
    f64,
    Option<Rect>,
    masonry_core::kurbo::Vec2,
    Affine,
    f64,
);
pub struct RawProbe {
    pub own_sample: Rc<RefCell<Option<OwnGeometry>>>,
    pub child: WidgetPod<StageGroup>,
    pub samples: Samples,
}
impl RawProbe {
    pub fn new(trace: Trace, samples: Samples) -> Self {
        Self {
            own_sample: Rc::new(RefCell::new(None)),
            child: StageGroup::new(trace, 1., Rect::new(0., 0., 20., 20.))
                .prepare()
                .to_pod(),
            samples,
        }
    }
}
impl Widget for RawProbe {
    type Action = NoAction;
    fn register_children(&mut self, c: &mut RegisterCtx<'_>) {
        c.register_child(&mut self.child);
    }
    fn on_pointer_event(
        &mut self,
        c: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        _: &PointerEvent,
    ) {
        *self.own_sample.borrow_mut() = Some((
            c.content_box(),
            c.border_box(),
            c.paint_box(),
            c.bounding_box(),
            c.first_baseline(),
            c.last_baseline(),
            c.clip_path(),
            c.border_box_translation(),
            c.window_transform(),
            c.scale_factor(),
        ));
        let raw = c
            .get_raw_mut(&mut self.child)
            .map(|(_, ctx)| ctx.transform());
        self.samples.borrow_mut().push(("pointer_mut", raw));
        let raw = c.get_raw(&mut self.child).map(|(_, ctx)| ctx.transform());
        self.samples.borrow_mut().push(("pointer_shared", raw));
        c.set_handled();
    }
    fn on_anim_frame(&mut self, c: &mut UpdateCtx<'_>, _: &mut PropertiesMut<'_>, _: u64) {
        let raw = c
            .get_raw_mut(&mut self.child)
            .map(|(_, ctx)| ctx.transform());
        self.samples.borrow_mut().push(("animation_mut", raw));
    }
    fn measure(
        &mut self,
        _: &mut MeasureCtx<'_>,
        _: &PropertiesRef<'_>,
        axis: Axis,
        _: LenReq,
        _: Option<Length>,
    ) -> Length {
        Length::px(if axis == Axis::Horizontal { 400. } else { 300. })
    }
    fn layout(&mut self, c: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, _: Size) {
        c.run_layout(&mut self.child, Size::new(20., 20.));
        c.place_child(&mut self.child, Point::new(10., 10.));
        c.set_clip_path(Rect::new(0., 0., 380., 280.));
        c.set_baselines(7., 11.);
        c.set_paint_insets(Insets::uniform(17.));
    }
    fn paint(&mut self, _: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, _: &mut Painter<'_>) {}
    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }
    fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, _: &mut Node) {}
    fn children_ids(&self) -> ChildrenIds {
        smallvec_children(self.child.id())
    }
}
fn smallvec_children(id: WidgetId) -> ChildrenIds {
    std::iter::once(id).collect()
}
