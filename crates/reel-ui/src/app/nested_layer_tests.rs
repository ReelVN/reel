use super::{layer_stack::LayerStack, *};
use crate::{
    core::*,
    layout::*,
    widgets::{Popup, PopupDismiss},
};
use accesskit::{Node, Role};
use kurbo::{Axis, Point, Size};
use std::{cell::RefCell, rc::Rc, sync::Arc};

struct Content;
impl Widget for Content {
    type Action = ();
    fn register_children(&mut self, _: &mut RegisterCtx<'_>) {}
    fn measure(
        &mut self,
        _: &mut MeasureCtx<'_>,
        _: &PropertiesRef<'_>,
        _: Axis,
        _: LenReq,
        _: Option<Length>,
    ) -> Length {
        Length::px(40.)
    }
    fn layout(&mut self, _: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, _: Size) {}
    fn paint(
        &mut self,
        _: &mut PaintCtx<'_>,
        _: &PropertiesRef<'_>,
        _: &mut crate::imaging::Painter<'_>,
    ) {
    }
    fn accessibility_role(&self) -> Role {
        Role::Button
    }
    fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, _: &mut Node) {}
    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::new()
    }
    fn accepts_focus(&self) -> bool {
        true
    }
}
fn down(x: f64, y: f64) -> PointerEvent {
    PointerEvent::Down(PointerButtonEvent {
        pointer: PointerInfo {
            pointer_id: Some(PointerId::PRIMARY),
            persistent_device_id: None,
            pointer_type: PointerType::Mouse,
        },
        state: PointerState {
            position: dpi::PhysicalPosition::new(x, y),
            ..Default::default()
        },
        button: Some(PointerButton::Primary),
    })
}
fn accept(root: &mut RenderRoot, presented: bool) {
    let (_, geometry) = root
        .record_presented_geometry(GeometryLimits { max_bytes: 65536 })
        .unwrap();
    if presented {
        root.set_input_geometry_source(InputGeometrySource::Presented(Some(Arc::new(geometry))));
    }
}

#[test]
fn nested_layer_capture_uses_registered_roots_and_accepted_geometry() {
    for presented in [false, true] {
        let base = Content.prepare();
        let base_id = base.id();
        let content = LayerStack::new(base).prepare();
        let content_id = content.id();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let sink = actions.clone();
        let mut root = RenderRoot::new(
            content,
            move |signal| {
                if let RenderRootSignal::Action(action, _) = signal
                    && let Some(dismiss) = action.downcast_ref::<PopupDismiss>()
                {
                    sink.borrow_mut().push(*dismiss);
                }
            },
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: dpi::PhysicalSize::new(200, 150),
                scale_factor: 1.,
                default_font: None,
            },
        );
        accept(&mut root, presented);
        assert!(root.global_state.input_layers.is_empty());
        root.focus_on(Some(base_id));
        // A Layer widget inside a registered layer is ordinary content. Its
        // capture hook must not be called a second time by the global dispatcher.
        let inner = Popup::new(Content.prepare()).with_modal(false).prepare();
        let popup = Popup::new(inner).prepare();
        let popup_id = popup.id();
        root.edit_widget(content_id, |mut widget| {
            LayerStack::add_layer(
                &mut widget.downcast::<LayerStack>(),
                popup,
                Point::new(80., 60.),
            );
        });
        accept(&mut root, presented);
        assert_eq!(root.global_state.input_layers, [popup_id]);
        root.handle_pointer_event(down(90., 70.));
        assert!(actions.borrow().is_empty());
        root.edit_widget(content_id, |mut widget| {
            LayerStack::reposition_layer(
                &mut widget.downcast::<LayerStack>(),
                popup_id,
                Point::new(130., 100.),
            );
        });
        if presented {
            root.handle_pointer_event(down(90., 70.));
            assert!(actions.borrow().is_empty());
        }
        accept(&mut root, presented);
        root.handle_pointer_event(down(10., 10.));
        assert_eq!(&*actions.borrow(), &[PopupDismiss::Outside]);
        root.edit_widget(content_id, |mut widget| {
            LayerStack::remove_layer(&mut widget.downcast::<LayerStack>(), popup_id);
        });
        assert!(root.global_state.input_layers.is_empty());
        root.handle_pointer_event(down(10., 10.));
        assert_eq!(
            actions.borrow().len(),
            1,
            "retired layers cannot capture old frames"
        );
        accept(&mut root, presented);
        assert_eq!(root.focused_widget(), Some(base_id));
    }
}
