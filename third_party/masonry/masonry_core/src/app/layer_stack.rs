// Copyright 2025 the Xilem Authors
// SPDX-License-Identifier: Apache-2.0

use std::any::TypeId;

use accesskit::{Node, Role};
use kurbo::{Axis, Point, Size};
use tracing::{Span, trace_span};

use crate::core::{
    AccessCtx, ChildrenIds, LayoutCtx, MeasureCtx, NewWidget, NoAction, PaintCtx, PropertiesRef,
    RegisterCtx, UpdateCtx, Widget, WidgetId, WidgetMut, WidgetPod,
};
use crate::imaging::Painter;
use crate::layout::{LenReq, Length, SizeDef};

/// A widget representing the top-level stack of visible layers owned by [`RenderRoot`](crate::app::RenderRoot).
///
/// This stack must always have at least one child, representing the "base layer",
/// e.g. most of the stuff drawn in the app.
///
/// Other layers can represent tooltips, menus, dialogs, etc.
/// They have an associated position and are drawn on top of the base layer.
///
/// Ensure that `LayerStack` has [`Dimensions`] set via props to [`Dimensions::MAX`].
/// Max preferred size of `LayerStack` means that the question of size
/// will get passed through to its base layer, and doesn't mean that it will
/// necessarily map to the max preferred size of the base layer.
///
/// [`Dimensions`]: crate::properties::Dimensions
/// [`Dimensions::MAX`]: crate::properties::Dimensions::MAX
pub(crate) struct LayerStack {
    layers: Vec<LayerEntry>,
}

/// A persistent entry in the top-level layer stack.
///
/// This is widget ownership state, not a derived visual/render layer.
struct LayerEntry {
    /// Root widget owned by this top-level layer entry.
    root: WidgetPod<dyn Widget>,
    /// Position of this layer entry in the `LayerStack`'s content-box coordinates.
    position: Point,
}

// --- MARK: IMPL LAYER_STACK
impl LayerStack {
    /// Creates a stack with the provided base layer.
    pub(crate) fn new(root: NewWidget<impl Widget + ?Sized>) -> Self {
        let layer = LayerEntry {
            root: root.erased().to_pod(),
            position: Point::ZERO,
        };
        Self {
            layers: vec![layer],
        }
    }

    /// Returns the number of layers, including the base layer.
    pub(crate) fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// The [`WidgetId`] of the root widget of the given layer.
    ///
    /// # Panics
    ///
    /// Panics if `idx` is out of bounds.
    pub(crate) fn layer_root_id(&self, idx: usize) -> WidgetId {
        self.layers[idx].root.id()
    }
}

// --- MARK: IMPL WIDGETMUT
impl LayerStack {
    /// Adds a new layer at the end of the stack, with the given widget as its root, at the given position.
    ///
    /// The given `pos` must be in this `LayerStack`'s content-box coordinate space.
    /// If this `LayerStack` is used as the root widget with no borders, padding, or transforms,
    /// then that coordinate space will exactly match the window's coordinate space.
    pub(crate) fn add_layer(
        this: &mut WidgetMut<'_, Self>,
        root: NewWidget<impl Widget + ?Sized>,
        position: Point,
    ) {
        let layer = LayerEntry {
            root: root.erased().to_pod(),
            position,
        };
        this.widget.layers.push(layer);
        this.ctx.children_changed();
    }

    /// Returns a mutable reference to the root widget of the layer at `idx`.
    ///
    /// # Panics
    ///
    /// Panics if `idx` is out of bounds.
    #[expect(dead_code, reason = "Might be useful later")]
    pub(crate) fn layer_root_mut<'t>(
        this: &'t mut WidgetMut<'_, Self>,
        idx: usize,
    ) -> Option<WidgetMut<'t, dyn Widget>> {
        let layer = &mut this.widget.layers.get_mut(idx)?.root;
        this.ctx.get_mut(layer)
    }

    /// Removes the layer with the given widget as root.
    ///
    /// The base layer cannot be removed.
    ///
    /// # Panics
    ///
    /// Panics in debug mode if the intended layer is the base layer or
    /// is not found.
    pub(crate) fn remove_layer(this: &mut WidgetMut<'_, Self>, root_id: WidgetId) {
        match this
            .widget
            .layers
            .iter()
            .position(|layer| layer.root.id() == root_id)
        {
            Some(0) => debug_panic!("Cannot remove initial layer"),
            None => debug_panic!("layer with root widget {root_id:?} not found"),
            Some(idx) => {
                let child = this.widget.layers.remove(idx).root;
                this.ctx.remove_child(child);
            }
        }
    }

    /// Repositions the layer with the given widget as root.
    ///
    /// The given `new_origin` must be in this `LayerStack`'s content-box coordinate space.
    /// If this `LayerStack` is used as the root widget with no borders, padding, or transforms,
    /// then that coordinate space will exactly match the window's coordinate space.
    ///
    /// The base layer cannot be repositioned.
    ///
    /// # Panics
    ///
    /// Panics in debug mode if the intended layer is the base layer or
    /// is not found.
    pub(crate) fn reposition_layer(
        this: &mut WidgetMut<'_, Self>,
        root_id: WidgetId,
        new_origin: Point,
    ) {
        match this
            .widget
            .layers
            .iter()
            .position(|layer| layer.root.id() == root_id)
        {
            Some(0) => debug_panic!("Cannot reposition initial layer"),
            None => debug_panic!("layer with root widget {root_id:?} not found"),
            Some(idx) => {
                this.widget.layers[idx].position = new_origin;
                this.ctx.request_layout();
            }
        }
    }
}

// --- MARK: IMPL WIDGET
impl Widget for LayerStack {
    type Action = NoAction;

    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        for layer in self.layers.iter_mut() {
            ctx.register_child(&mut layer.root);
        }
    }

    fn property_changed(&mut self, _ctx: &mut UpdateCtx<'_>, _property_type: TypeId) {}

    fn measure(
        &mut self,
        ctx: &mut MeasureCtx<'_>,
        _props: &PropertiesRef<'_>,
        axis: Axis,
        _len_req: LenReq,
        cross_length: Option<Length>,
    ) -> Length {
        // First child is the base layer, for which we do measure passthrough.
        let Some(base_layer) = self.layers.first_mut() else {
            debug_panic!("Missing first layer");
            return Length::ZERO;
        };
        // Let the base layer handle the response
        ctx.redirect_measurement(&mut base_layer.root, axis, cross_length)
    }

    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _props: &PropertiesRef<'_>, size: Size) {
        // First child is the base layer, for which we do layout passthrough.
        let Some(base_layer) = self.layers.first_mut() else {
            debug_panic!("Missing first layer");
            return;
        };

        // Our measurement is just the passed on result from the base layer,
        // so the size we received is effectively meant for the base layer.
        ctx.run_layout(&mut base_layer.root, size);
        // The base layer is always located at our origin.
        ctx.place_child(&mut base_layer.root, Point::ORIGIN);

        ctx.derive_baselines(&base_layer.root);

        for layer in &mut self.layers[1..] {
            // Other layers don't take part in our measurement,
            // so RenderRoot has no idea what they want and instead we control them.
            // We don't really care if they go outside the bounds of the base layer,
            // so we won't give any FitContent fallback and instead just use MaxContent.
            // These other layers still have access to the window size via context size.
            let layer_size = ctx.compute_size(&mut layer.root, SizeDef::MAX, size.into());
            ctx.run_layout(&mut layer.root, layer_size);
            ctx.place_child(&mut layer.root, layer.position);
        }
    }

    fn paint(
        &mut self,
        _ctx: &mut PaintCtx<'_>,
        _props: &PropertiesRef<'_>,
        _painter: &mut Painter<'_>,
    ) {
    }

    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }

    fn accessibility(
        &mut self,
        _ctx: &mut AccessCtx<'_>,
        _props: &PropertiesRef<'_>,
        _node: &mut Node,
    ) {
    }

    fn children_ids(&self) -> ChildrenIds {
        self.layers.iter().map(|child| child.root.id()).collect()
    }

    fn make_trace_span(&self, id: WidgetId) -> Span {
        trace_span!("LayerStack", id = id.trace())
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;
    use crate::app::{
        GeometryLimits, InputGeometrySource, RenderRoot, RenderRootOptions, RenderRootSignal,
        WindowSizePolicy,
    };
    use crate::core::*;
    use dpi::PhysicalSize;
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        sync::Arc,
    };

    #[derive(Debug)]
    struct Probe {
        hooks: Rc<Cell<usize>>,
        cancels: Rc<Cell<usize>>,
    }
    impl Widget for Probe {
        type Action = u8;
        fn register_children(&mut self, _: &mut RegisterCtx<'_>) {}
        fn measure(
            &mut self,
            _: &mut MeasureCtx<'_>,
            _: &PropertiesRef<'_>,
            _: Axis,
            _: LenReq,
            _: Option<Length>,
        ) -> Length {
            Length::px(100.)
        }
        fn layout(&mut self, _: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, _: Size) {}
        fn paint(&mut self, _: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, _: &mut Painter<'_>) {}
        fn accessibility_role(&self) -> Role {
            Role::GenericContainer
        }
        fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, _: &mut Node) {}
        fn children_ids(&self) -> ChildrenIds {
            ChildrenIds::new()
        }
        fn as_layer(&mut self) -> Option<&mut dyn Layer> {
            Some(self)
        }
        fn on_pointer_event(
            &mut self,
            ctx: &mut EventCtx<'_>,
            _: &mut PropertiesMut<'_>,
            event: &PointerEvent,
        ) {
            match event {
                PointerEvent::Down(_) => ctx.capture_pointer(),
                PointerEvent::Cancel(_) => {
                    assert!(
                        ctx.is_pointer_capture_target(),
                        "cleanup handler retains capture"
                    );
                    self.cancels.set(self.cancels.get() + 1);
                }
                _ => {}
            }
        }
    }
    impl Layer for Probe {
        fn capture_pointer_event(
            &mut self,
            ctx: &mut EventCtx<'_>,
            _: &mut PropertiesMut<'_>,
            event: &PointerEvent,
        ) {
            if matches!(event, PointerEvent::Down(_)) {
                self.hooks.set(self.hooks.get() + 1);
                ctx.submit_action::<Self::Action>(1);
            }
        }
    }
    fn down() -> PointerEvent {
        PointerEvent::Down(PointerButtonEvent {
            pointer: PointerInfo {
                pointer_id: Some(PointerId::PRIMARY),
                persistent_device_id: None,
                pointer_type: PointerType::Mouse,
            },
            state: PointerState {
                position: dpi::PhysicalPosition::new(20., 20.),
                ..Default::default()
            },
            button: Some(PointerButton::Primary),
        })
    }
    #[test]
    fn layer_hooks_obey_current_disabled_and_stashed_with_terminal_cleanup() {
        for presented in [false, true] {
            for stashed in [false, true] {
                let hooks = Rc::new(Cell::new(0));
                let cancels = Rc::new(Cell::new(0));
                let signals = Rc::new(RefCell::new(vec![]));
                let sink = signals.clone();
                let widget = Probe {
                    hooks: hooks.clone(),
                    cancels: cancels.clone(),
                }
                .prepare();
                let layer = widget.id();
                let mut root = RenderRoot::new(
                    widget,
                    move |s| sink.borrow_mut().push(s),
                    RenderRootOptions {
                        default_properties: Arc::new(DefaultProperties::new()),
                        use_system_fonts: false,
                        size_policy: WindowSizePolicy::User,
                        size: PhysicalSize::new(100, 100),
                        scale_factor: 1.,
                        test_font: None,
                    },
                );
                let (_, geometry) = root
                    .record_presented_geometry(GeometryLimits {
                        max_nodes: 8,
                        max_bytes: 4096,
                        max_path: 8,
                    })
                    .unwrap();
                if presented {
                    root.set_input_geometry_source(InputGeometrySource::Presented(Some(Arc::new(
                        geometry,
                    ))));
                }
                root.handle_pointer_event(down());
                assert_eq!(hooks.get(), 1);
                assert_eq!(root.pointer_capture_target(), Some(layer));
                hooks.set(0);
                if stashed {
                    root.edit_widget(root.root_id(), |mut w| {
                        let mut stack = w.downcast::<LayerStack>();
                        stack
                            .ctx
                            .set_stashed(&mut stack.widget.layers[0].root, true);
                    });
                } else {
                    root.edit_widget(layer, |mut w| w.ctx.set_disabled(true));
                }
                assert_eq!(cancels.get(), 1);
                assert_eq!(root.pointer_capture_target(), None);
                signals.borrow_mut().clear();
                root.handle_pointer_event(down());
                assert_eq!(
                    hooks.get(),
                    0,
                    "ineligible layer hook: presented={presented} stashed={stashed}"
                );
                assert!(
                    !signals
                        .borrow()
                        .iter()
                        .any(|s| matches!(s, RenderRootSignal::Action(..)))
                );
                assert_eq!(cancels.get(), 1);
            }
        }
    }
}
