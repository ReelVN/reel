//! A renderer/input diagnostic, not an Aside story session.
use masonry_core::{
    accesskit::{Node, Role},
    core::*,
    imaging::{Composite, GroupRef, Painter},
    kurbo::{Affine, Axis, Point, Rect, Size},
    layout::{LenReq, Length},
    peniko::{Color, ImageData},
};

pub struct Diagnostic {
    x: f64,
    dragging: bool,
    image: ImageData,
    label: Option<WidgetPod<masonry::widgets::Label>>,
}
impl Diagnostic {
    pub fn new(image: ImageData, text: bool) -> Self {
        Self {
            x: 140.,
            dragging: false,
            image,
            label: text.then(|| {
                WidgetPod::new(masonry::widgets::Label::new(
                    "Reel: drag to move; scroll to zoom. GPU + Parley diagnostic.",
                ))
            }),
        }
    }
}
impl Widget for Diagnostic {
    type Action = ();
    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        let position = match event {
            PointerEvent::Down(e) => {
                self.dragging = true;
                ctx.capture_pointer();
                Some(e.state.position)
            }
            PointerEvent::Move(e) if self.dragging => Some(e.current.position),
            PointerEvent::Up(_) | PointerEvent::Cancel(_) => {
                self.dragging = false;
                ctx.release_pointer();
                None
            }
            _ => None,
        };
        if let Some(position) = position {
            self.x = ctx.local_position(position).x.clamp(40., 520.);
            ctx.request_paint_only();
            ctx.set_handled();
        }
    }
    fn on_text_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &TextEvent,
    ) {
        if matches!(event, TextEvent::WindowFocusChange(false)) {
            self.dragging = false;
            ctx.release_pointer();
        }
    }
    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        if let Some(label) = &mut self.label {
            ctx.register_child(label);
        }
    }
    fn measure(
        &mut self,
        _: &mut MeasureCtx<'_>,
        _: &PropertiesRef<'_>,
        axis: Axis,
        _: LenReq,
        _: Option<Length>,
    ) -> Length {
        Length::px(if axis == Axis::Horizontal { 640. } else { 420. })
    }
    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, _: Size) {
        if let Some(label) = &mut self.label {
            ctx.run_layout(label, Size::new(560., 50.));
            ctx.place_child(label, Point::new(40., 350.));
        }
    }
    fn paint(&mut self, _: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, painter: &mut Painter<'_>) {
        painter.fill_rect(
            Rect::new(0., 0., 1000., 1000.),
            Color::from_rgb8(18, 23, 35),
        );
        painter.fill_rect(
            Rect::new(40., 195., 560., 205.),
            Color::from_rgb8(100, 120, 140),
        );
        painter.push_fill_clip(Rect::new(40., 40., 560., 330.));
        painter.push_group(GroupRef {
            composite: Composite {
                alpha: 0.65,
                ..Default::default()
            },
            ..Default::default()
        });
        painter.fill_rect(
            Rect::new(self.x - 40., 100., self.x + 100., 260.),
            Color::from_rgb8(240, 60, 60),
        );
        painter.fill_rect(
            Rect::new(self.x + 30., 150., self.x + 170., 290.),
            Color::from_rgb8(40, 100, 240),
        );
        painter.pop_group();
        painter.pop_clip();
        painter.draw_image(
            &self.image,
            Affine::translate((440., 70.)) * Affine::scale(30.),
        );
        painter.fill_rect(Rect::new(30., 340., 610., 405.), Color::WHITE);
        painter.fill_rect(
            Rect::new(self.x - 5., 180., self.x + 5., 220.),
            Color::WHITE,
        );
    }
    fn accessibility_role(&self) -> Role {
        Role::Slider
    }
    fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, node: &mut Node) {
        node.set_label("Renderer diagnostic: drag to move overlap");
    }
    fn children_ids(&self) -> ChildrenIds {
        self.label.iter().map(|l| l.id()).collect()
    }
    fn accepts_pointer_interaction(&self) -> bool {
        true
    }
}
