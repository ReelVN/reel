// Copyright 2020 the Xilem Authors and the Druid Authors
// SPDX-License-Identifier: Apache-2.0

//! The context types that are passed into various widget methods.

#![expect(
    missing_debug_implementations,
    reason = "Not sure if it's worth the effort"
)]

use std::any::{Any, TypeId};
use std::collections::hash_map::Entry;

use crate::tree_arena::{ArenaMut, ArenaMutList, ArenaRefList};
use accesskit::{NodeId, TreeUpdate};
use anymore::AnyDebug;
use dpi::{LogicalPosition, PhysicalPosition};
use reel_text::{FontContext, LayoutContext};
use tracing::{trace, warn};

use crate::app::{MutateCallback, RenderRootSignal, RenderRootState};
use crate::core::{
    AllowRawMut, BrushIndex, ClassSet, ErasedAction, FromDynWidget, LayerType, NewWidget,
    PaintImageMask, PaintLayerMode, PropertiesMut, PropertiesRef, PropertyArena, PropertyCache,
    PropertyStackId, ResizeDirection, Widget, WidgetArenaNode, WidgetId, WidgetMut, WidgetPod,
    WidgetRef, WidgetState,
};
use crate::kurbo::{Affine, Axis, Insets, Point, Rect, Size, Vec2};
use crate::layout::{LayoutSize, LenDef, Length, SizeDef};
use crate::passes::layout::{place_widget, resolve_length, resolve_size, run_layout_on};
use crate::peniko::Color;
use crate::util::{ParentLinkedList, get_debug_color};

// Note - Most methods defined in this file revolve around `WidgetState` fields.
// Consider reading `WidgetState` documentation (especially the documented naming scheme)
// before editing context method code.

/// A macro for implementing methods on multiple contexts.
///
/// There are a lot of methods defined on multiple contexts; this lets us only
/// have to write them out once.
macro_rules! impl_context_method {
    ($ty:ty,  { $($method:item)+ } ) => {
        impl $ty { $($method)+ }
    };
    ( $ty:ty, $($more:ty),+, { $($method:item)+ } ) => {
        impl_context_method!($ty, { $($method)+ });
        impl_context_method!($($more),+, { $($method)+ });
    };
}

/// A context provided inside of [`WidgetMut`].
///
/// When you declare a mutable reference type for your widget, methods of this type
/// will have access to a `MutateCtx`. If that method mutates the widget in a way that
/// requires a later pass (for instance, if your widget has a `set_color` method),
/// you will need to signal that change in the pass (eg [`request_render`](MutateCtx::request_render)).
pub struct MutateCtx<'a> {
    pub(crate) geometry: crate::app::ContextGeometry<'a>,
    pub(crate) global_state: &'a mut RenderRootState,
    pub(crate) parent_widget_state: Option<&'a mut WidgetState>,
    pub(crate) widget_state: &'a mut WidgetState,
    pub(crate) properties: PropertiesMut<'a>,
    pub(crate) children: ArenaMutList<'a, WidgetArenaNode>,
    pub(crate) property_arena: &'a PropertyArena,
}

/// A context provided to the [`Widget::on_action`] method.
pub struct ActionCtx<'a> {
    pub(crate) geometry: crate::app::ContextGeometry<'a>,
    pub(crate) global_state: &'a mut RenderRootState,
    pub(crate) widget_state: &'a mut WidgetState,
    pub(crate) children: ArenaMutList<'a, WidgetArenaNode>,
    pub(crate) property_arena: &'a PropertyArena,
    pub(crate) is_handled: bool,
}

/// A context provided inside of [`WidgetRef`].
///
/// This context is passed to methods of widgets requiring shared, read-only access.
#[derive(Clone, Copy)]
pub struct QueryCtx<'a> {
    pub(crate) geometry: crate::app::ContextGeometry<'a>,
    pub(crate) global_state: &'a RenderRootState,
    pub(crate) widget_state: &'a WidgetState,
    pub(crate) properties: PropertiesRef<'a>,
    pub(crate) children: ArenaRefList<'a, WidgetArenaNode>,
    pub(crate) property_arena: &'a PropertyArena,
}

/// A context given when calling another context's `get_raw_mut()` method.
pub struct RawCtx<'a> {
    pub(crate) geometry: crate::app::ContextGeometry<'a>,
    pub(crate) global_state: &'a mut RenderRootState,
    pub(crate) parent_widget_state: &'a mut WidgetState,
    pub(crate) widget_state: &'a mut WidgetState,
    pub(crate) children: ArenaMutList<'a, WidgetArenaNode>,
    pub(crate) property_arena: &'a PropertyArena,
}

/// A context provided to event-handling [`Widget`] methods.
pub struct EventCtx<'a> {
    pub(crate) geometry: crate::app::ContextGeometry<'a>,
    pub(crate) global_state: &'a mut RenderRootState,
    pub(crate) widget_state: &'a mut WidgetState,
    pub(crate) children: ArenaMutList<'a, WidgetArenaNode>,
    pub(crate) property_arena: &'a PropertyArena,
    pub(crate) target: WidgetId,
    pub(crate) allow_pointer_capture: bool,
    pub(crate) is_handled: bool,
    pub(crate) propagation_stopped: bool,
}

/// A context provided to the [`Widget::register_children`] method.
pub struct RegisterCtx<'a> {
    pub(crate) global_state: &'a mut RenderRootState,
    pub(crate) children: ArenaMutList<'a, WidgetArenaNode>,
    #[cfg(debug_assertions)]
    pub(crate) registered_ids: Vec<WidgetId>,
}

/// A context provided to the [`Widget::update`] method.
pub struct UpdateCtx<'a> {
    pub(crate) geometry: crate::app::ContextGeometry<'a>,
    pub(crate) global_state: &'a mut RenderRootState,
    pub(crate) widget_state: &'a mut WidgetState,
    pub(crate) children: ArenaMutList<'a, WidgetArenaNode>,
    pub(crate) ancestors: Option<&'a ParentLinkedList<'a>>,
    pub(crate) property_arena: &'a PropertyArena,
}

/// A context provided to [`Widget::measure`] methods.
pub struct MeasureCtx<'a> {
    pub(crate) geometry: crate::app::ContextGeometry<'a>,
    pub(crate) global_state: &'a mut RenderRootState,
    pub(crate) widget_state: &'a mut WidgetState,
    pub(crate) children: ArenaMutList<'a, WidgetArenaNode>,
    pub(crate) property_arena: &'a PropertyArena,
    pub(crate) auto_length: LenDef,
    pub(crate) context_size: LayoutSize,
    pub(crate) cache_result: bool,
}

/// A context provided to [`Widget::layout`] methods.
pub struct LayoutCtx<'a> {
    pub(crate) geometry: crate::app::ContextGeometry<'a>,
    pub(crate) global_state: &'a mut RenderRootState,
    pub(crate) widget_state: &'a mut WidgetState,
    pub(crate) children: ArenaMutList<'a, WidgetArenaNode>,
    pub(crate) property_arena: &'a PropertyArena,
}

/// A context provided to the [`Widget::compose`] method.
pub struct ComposeCtx<'a> {
    pub(crate) geometry: crate::app::ContextGeometry<'a>,
    pub(crate) global_state: &'a mut RenderRootState,
    pub(crate) widget_state: &'a mut WidgetState,
    pub(crate) children: ArenaMutList<'a, WidgetArenaNode>,
    pub(crate) property_arena: &'a PropertyArena,
}

/// A context passed to [`Widget::paint`] method.
pub struct PaintCtx<'a> {
    pub(crate) geometry: crate::app::ContextGeometry<'a>,
    pub(crate) global_state: &'a mut RenderRootState,
    pub(crate) widget_state: &'a mut WidgetState,
    pub(crate) children: ArenaMutList<'a, WidgetArenaNode>,
    pub(crate) image_draws: &'a mut Vec<super::recorded_paint::ResourceDraw>,
    pub(crate) command_cursor: &'a std::cell::Cell<usize>,
}

/// A context passed to [`Widget::accessibility`] method.
pub struct AccessCtx<'a> {
    pub(crate) geometry: crate::app::ContextGeometry<'a>,
    pub(crate) global_state: &'a mut RenderRootState,
    pub(crate) widget_state: &'a mut WidgetState,
    pub(crate) children: ArenaMutList<'a, WidgetArenaNode>,
    pub(crate) tree_update: &'a mut TreeUpdate,
}

impl_context_method!(
    MutateCtx<'_>,
    ActionCtx<'_>,
    QueryCtx<'_>,
    EventCtx<'_>,
    UpdateCtx<'_>,
    RawCtx<'_>,
    MeasureCtx<'_>,
    LayoutCtx<'_>,
    ComposeCtx<'_>,
    PaintCtx<'_>,
    AccessCtx<'_>,
    {
        fn geometry_state(&self) -> crate::app::GeometryRef<'_> {
            self.geometry.resolve(self.widget_state)
        }
    }
);

// --- MARK: GETTERS
// Methods for all context types
impl_context_method!(
    MutateCtx<'_>,
    ActionCtx<'_>,
    QueryCtx<'_>,
    EventCtx<'_>,
    UpdateCtx<'_>,
    MeasureCtx<'_>,
    LayoutCtx<'_>,
    ComposeCtx<'_>,
    PaintCtx<'_>,
    AccessCtx<'_>,
    RawCtx<'_>,
    {
        /// The `WidgetId` of the current widget.
        pub fn widget_id(&self) -> WidgetId {
            self.widget_state.id
        }

        #[allow(dead_code, reason = "Copy-pasted for some types that don't need it")]
        /// Helper method to get a direct reference to a child widget from its `WidgetPod`.
        fn get_child<Child: Widget>(&self, child: &'_ WidgetPod<Child>) -> &'_ Child {
            let child_ref = &*self
                .children
                .item(child.id())
                .expect("get_child: child not found")
                .item
                .widget;
            (child_ref as &dyn Any).downcast_ref::<Child>().unwrap()
        }

        #[allow(dead_code, reason = "Copy-pasted for some types that don't need it")]
        /// Helper method to get a direct reference to a child widget from its `WidgetPod`.
        fn get_child_dyn(&self, child: &'_ WidgetPod<impl Widget + ?Sized>) -> &'_ dyn Widget {
            &*self
                .children
                .item(child.id())
                .expect("get_child: child not found")
                .item
                .widget
        }

        #[allow(dead_code, reason = "Copy-pasted for some types that don't need it")]
        /// Helper method to get a direct reference to a child widget's `WidgetState` from its `WidgetPod`.
        fn get_child_state(&self, child: &'_ WidgetPod<impl Widget + ?Sized>) -> &'_ WidgetState {
            &self
                .children
                .item(child.id())
                .expect("get_child_state: child not found")
                .item
                .state
        }

        #[allow(dead_code, reason = "Copy-pasted for some types that don't need it")]
        /// Returns the local transform of this widget.
        ///
        /// This transform is used during the mapping of this widget's border-box coordinate space
        /// to the parent's border-box coordinate space.
        ///
        /// This transform is applied before `scroll_translation` and `origin`.
        pub fn transform(&self) -> Affine {
            self.geometry_state().transform()
        }
    }
);

// TODO - Merge into other block?
impl_context_method!(
    MutateCtx<'_>,
    ActionCtx<'_>,
    EventCtx<'_>,
    UpdateCtx<'_>,
    MeasureCtx<'_>,
    LayoutCtx<'_>,
    ComposeCtx<'_>,
    PaintCtx<'_>,
    AccessCtx<'_>,
    RawCtx<'_>,
    {
        /// Returns a mutable reference to this widget's property cache.
        pub fn property_cache(&mut self) -> &mut PropertyCache {
            &mut self.widget_state.property_cache
        }
    }
);

impl QueryCtx<'_> {
    /// Returns a reference to this widget's property cache.
    pub fn property_cache(&self) -> &PropertyCache {
        &self.widget_state.property_cache
    }
}

impl_context_method!(
    MutateCtx<'_>,
    ActionCtx<'_>,
    EventCtx<'_>,
    UpdateCtx<'_>,
    MeasureCtx<'_>,
    LayoutCtx<'_>,
    ComposeCtx<'_>,
    RawCtx<'_>,
    {
        /// Helper method to get a mutable reference to a child widget's `WidgetState` from its `WidgetPod`.
        ///
        /// This one isn't defined for `PaintCtx` and `AccessCtx` because those contexts
        /// can't mutate `WidgetState`.
        #[track_caller]
        fn get_child_state_mut<Child: Widget + ?Sized>(
            &mut self,
            child: &'_ mut WidgetPod<Child>,
        ) -> &'_ mut WidgetState {
            &mut self
                .children
                .item_mut(child.id())
                .expect("get_child_state_mut: child not found")
                .item
                .state
        }
    }
);

// --- MARK: WIDGET_MUT
// Methods to get a child WidgetMut from a parent.
impl MutateCtx<'_> {
    /// Returns a [`WidgetMut`] to a child widget.
    pub fn get_mut<'c, Child: Widget + FromDynWidget + ?Sized>(
        &'c mut self,
        child: &'c mut WidgetPod<Child>,
    ) -> Option<WidgetMut<'c, Child>> {
        let node_mut = self.children.item_mut(child.id())?;
        let child_stack_id = node_mut.item.state.property_stack_id;
        let child_type_id = (*node_mut.item.widget).type_id();
        let child_stack = self.property_arena.get(child_stack_id, child_type_id);
        let child_ctx = MutateCtx {
            geometry: crate::app::ContextGeometry::Live,
            global_state: self.global_state,
            parent_widget_state: Some(&mut self.widget_state),
            widget_state: &mut node_mut.item.state,
            properties: PropertiesMut {
                local: &mut node_mut.item.properties,
                default_map: self.properties.default_map,
                stack: child_stack,
                class_set: &node_mut.item.class_set,
            },
            children: node_mut.children,
            property_arena: self.property_arena,
        };
        Some(WidgetMut {
            ctx: child_ctx,
            widget: Child::from_dyn_mut(&mut *node_mut.item.widget)?,
        })
    }

    pub(crate) fn reborrow_mut(&mut self) -> MutateCtx<'_> {
        MutateCtx {
            geometry: crate::app::ContextGeometry::Live,
            global_state: self.global_state,
            // We don't don't reborrow `parent_widget_state`. This avoids running
            // `merge_up` in `WidgetMut::Drop` multiple times for the same state.
            // It will still be called when the original borrow is dropped.
            parent_widget_state: None,
            widget_state: self.widget_state,
            properties: PropertiesMut {
                local: &mut *self.properties.local,
                default_map: self.properties.default_map,
                stack: self.properties.stack,
                class_set: self.properties.class_set,
            },
            children: self.children.reborrow_mut(),
            property_arena: self.property_arena,
        }
    }

    pub(crate) fn update_mut(&mut self) -> UpdateCtx<'_> {
        UpdateCtx {
            geometry: self.geometry,
            global_state: self.global_state,
            widget_state: self.widget_state,
            children: self.children.reborrow_mut(),
            ancestors: None,
            property_arena: self.property_arena,
        }
    }

    /// Sets which property stack this widget uses for property resolution.
    pub fn set_property_stack(&mut self, stack_id: PropertyStackId) {
        self.widget_state.request_update_props = true;
        self.widget_state.needs_update_props = true;
        self.widget_state.property_cache.invalidated = true;
        self.widget_state.property_stack_id = Some(stack_id);
    }
}

// --- MARK: WIDGET_REF
// Methods to get a child WidgetRef from a parent.
impl<'w> QueryCtx<'w> {
    /// Returns a [`WidgetRef`] to a child widget.
    pub fn get(self, child: WidgetId) -> Option<WidgetRef<'w, dyn Widget>> {
        let geometry = self.geometry.child(child)?;
        let child_node = self.children.item(child)?;
        let child_type_id = (*child_node.item.widget).type_id();
        let child_stack = self
            .property_arena
            .get(child_node.item.state.property_stack_id, child_type_id);
        let child_ctx = QueryCtx {
            geometry,
            global_state: self.global_state,
            widget_state: &child_node.item.state,
            properties: PropertiesRef {
                local: &child_node.item.properties,
                default_map: self.properties.default_map,
                stack: child_stack,
                class_set: &child_node.item.class_set,
            },
            children: child_node.children,
            property_arena: self.property_arena,
        };
        Some(WidgetRef {
            ctx: child_ctx,
            widget: &*child_node.item.widget,
        })
    }
}

// Methods for all exclusive context types (i.e. those which have exclusive access to the global state).
impl_context_method!(
    MutateCtx<'_>,
    ActionCtx<'_>,
    EventCtx<'_>,
    UpdateCtx<'_>,
    MeasureCtx<'_>,
    LayoutCtx<'_>,
    ComposeCtx<'_>,
    PaintCtx<'_>,
    AccessCtx<'_>,
    RawCtx<'_>,
    {
        /// Returns the Parley contexts needed to build and paint text sections.
        ///
        /// Most users should embed a pre-built label widget
        /// (such as `crate::widgets::Label`)
        /// as a child for non-interactive text.
        /// These contexts could however be useful for custom text editing, such as for rich text editing.
        ///
        /// Any cached text layouts should be invalidated when receiving [`Update::FontsChanged`].
        ///
        /// [`Update::FontsChanged`]: crate::core::Update::FontsChanged
        pub fn text_contexts(&mut self) -> (&mut FontContext, &mut LayoutContext<BrushIndex>) {
            (
                &mut self.global_state.font_context,
                &mut self.global_state.text_layout_context,
            )
        }
    }
);

// --- MARK: EVENT HANDLING
impl EventCtx<'_> {
    /// Stops delivery to ancestors without consuming the event. The host may
    /// still use an unhandled physical input for semantic navigation.
    pub fn stop_propagation(&mut self) {
        self.propagation_stopped = true;
    }

    /// Whether this event uses the current interaction configuration.
    pub fn input_configuration_is_current(&self) -> bool {
        self.geometry_state().input_revision() == self.widget_state.input_revision
    }

    /// Captures the pointer in the current widget.
    ///
    /// [Pointer capture] is only allowed during a [`Down`] event. It is a logic error to
    /// capture the pointer during any other event.
    ///
    /// A widget normally only receives pointer events when the pointer is inside the widget's
    /// border-box. Pointer capture causes border-box hit checks to be ignored: when the pointer is
    /// captured by a widget, that widget will continue receiving pointer events when the pointer
    /// is outside the widget's border-box. Other widgets the pointer is over will not receive
    /// events. Events that are not marked as handled by the capturing widget, bubble up to the
    /// widget's ancestors, ignoring their border-boxes as well.
    ///
    /// The pointer cannot be captured by multiple widgets at the same time. If a widget has
    /// captured the pointer and another widget captures it, the first widget loses the pointer
    /// capture.
    ///
    /// # Releasing the pointer
    ///
    /// Any widget can [`release`] the pointer during any event.
    /// The pointer is automatically released after handling of a [`Up`] or [`Cancel`] event completes.
    /// A widget holding the pointer capture will be the target of these events.
    ///
    /// If pointer capture is lost for external reasons (the widget is disabled, the window lost focus,
    /// etc), the widget will still get a [`Cancel`] event.
    ///
    /// [Pointer capture]: crate::doc::concepts#pointer-capture
    /// [`Down`]: ui_events::pointer::PointerEvent::Down
    /// [`Up`]: ui_events::pointer::PointerEvent::Up
    /// [`Cancel`]: ui_events::pointer::PointerEvent::Cancel
    /// [`release`]: Self::release_pointer
    #[track_caller]
    pub fn capture_pointer(&mut self) {
        let id = self.widget_id();
        if !self.allow_pointer_capture {
            debug_panic!("capture_pointer - '{id}': event does not allow pointer capture");
            return;
        }
        // TODO: plumb pointer capture through to platform (through winit)
        self.global_state.pointer.pointer_capture_target = Some(id);
        self.global_state.pointer.pointer_capture_allows_pan = false;
        self.global_state.pointer.pointer_capture_revision = self.widget_state.input_revision;
        self.global_state.needs_pointer_pass = true;
    }

    /// Captures a click while allowing an ancestor to turn a touch drag into scrolling.
    pub fn capture_pointer_for_click(&mut self) {
        self.capture_pointer();
        self.global_state.pointer.pointer_capture_allows_pan = true;
    }

    /// Releases the pointer previously [captured] through [`capture_pointer`].
    ///
    /// [captured]: crate::doc::concepts#pointer-capture
    /// [`capture_pointer`]: EventCtx::capture_pointer
    pub fn release_pointer(&mut self) {
        let id = self.widget_id();
        if self.global_state.pointer.pointer_capture_target.is_none() {
            warn!("release_pointer - '{id}': no widget is captured");
            return;
        }
        if self.global_state.pointer.pointer_capture_target != Some(self.widget_state.id) {
            warn!("release_pointer - '{id}': widget does not have pointer capture");
            return;
        }
        self.global_state.pointer.pointer_capture_target = None;
        self.global_state.needs_pointer_pass = true;
    }

    /// The widget originally targeted by the event.
    ///
    /// This will be different from [`widget_id`](Self::widget_id) during event bubbling.
    pub fn target(&self) -> WidgetId {
        self.target
    }

    /// Converts the given position from the window's coordinate space
    /// to this widget's content-box coordinate space.
    pub fn local_position(&self, p: PhysicalPosition<f64>) -> Point {
        let LogicalPosition { x, y } = p.to_logical(self.scale_factor());
        self.to_local(Point { x, y })
    }
}

// Methods shared by event and action handling.
impl_context_method!(ActionCtx<'_>, EventCtx<'_>, {
    /// Sends a signal to parent widgets to scroll this widget's border-box into view.
    pub fn request_scroll_to_this(&mut self) {
        let rect = self.widget_state.border_box();
        self.global_state
            .scroll_request_targets
            .push((self.widget_state.id, rect));
    }

    /// Sends a signal to parent widgets to scroll the provided `rect` into view.
    ///
    /// The `rect` must be in this widget's content-box coordinate space.
    pub fn request_scroll_to(&mut self, rect: Rect) {
        // Convert from this widget's content-box space to border-box space.
        let rect = rect + self.widget_state.border_box_translation();
        self.global_state
            .scroll_request_targets
            .push((self.widget_state.id, rect));
    }

    /// Sets the event as "handled", which stops its propagation to parent
    /// widgets.
    pub fn set_handled(&mut self) {
        trace!("set_handled");
        self.is_handled = true;
    }

    /// Determines whether the event has been handled.
    pub fn is_handled(&self) -> bool {
        self.is_handled
    }

    /// Requests [text focus].
    ///
    /// Because only one widget can be focused at a time, multiple focus requests
    /// from different widgets during a single event cycle means that the last
    /// widget that requests focus will override the previous requests.
    ///
    /// [text focus]: crate::doc::concepts#text-focus
    pub fn request_focus(&mut self) {
        trace!("request_focus");
        // We need to send the request even if we're currently focused,
        // because we may have a sibling widget that already requested focus
        // and we have no way of knowing that yet. We need to override that
        // to deliver on the "last focus request wins" promise.
        let id = self.widget_id();
        self.global_state.next_focused_widget = Some(id);
    }

    /// Transfers [text focus] to the widget with the given `WidgetId`.
    ///
    /// [text focus]: crate::doc::concepts#text-focus
    pub fn set_focus(&mut self, target: WidgetId) {
        trace!("set_focus target={:?}", target);
        self.global_state.next_focused_widget = Some(target);
    }

    /// Gives up [text focus].
    ///
    /// This should only be called by a widget that currently has focus.
    ///
    /// [text focus]: crate::doc::concepts#text-focus
    pub fn resign_focus(&mut self) {
        trace!("resign_focus");
        if self.has_focus_target() {
            self.global_state.next_focused_widget = None;
        } else {
            warn!(
                "resign_focus can only be called by the currently focused widget {} \
                 or one of its ancestors.",
                self.widget_id()
            );
        }
    }
});

// --- MARK: ACCESSIBILITY
impl AccessCtx<'_> {
    // TODO - We need access to the TreeUpdate to create sub-nodes for text runs,
    // but this seems too powerful. We should figure out another API.
    /// A mutable reference to the global [`TreeUpdate`] object in which all modified/new
    /// accessibility nodes are stored.
    pub fn tree_update(&mut self) -> &mut TreeUpdate {
        self.tree_update
    }

    /// Returns an id which is guaranteed not to collide with [`WidgetId`]s or with previous ids returned by this function.
    pub fn next_node_id() -> NodeId {
        // TODO - Return from a pool disjoint from widget ids.
        WidgetId::next().into()
    }
}

mod layout;

// --- MARK: GET GEOMETRY
// Methods on all context types except MeasureCtx and LayoutCtx
// These methods access geometry resolved during layout and compose.
impl_context_method!(
    MutateCtx<'_>,
    ActionCtx<'_>,
    QueryCtx<'_>,
    EventCtx<'_>,
    UpdateCtx<'_>,
    ComposeCtx<'_>,
    PaintCtx<'_>,
    AccessCtx<'_>,
    {
        /// Returns the aligned content-box rect of this widget
        /// in this widget's content-box coordinate space.
        pub fn content_box(&self) -> Rect {
            let translation = self.geometry_state().border_box_translation();
            self.geometry_state().content_box() - translation
        }

        /// Returns the aligned border-box rect of this widget
        /// in this widget's content-box coordinate space.
        pub fn border_box(&self) -> Rect {
            let translation = self.geometry_state().border_box_translation();
            self.geometry_state().border_box() - translation
        }

        /// Returns the aligned paint-box rect of this widget
        /// in this widget's content-box coordinate space.
        ///
        /// Covers the area we expect to be invalidated when the widget is painted.
        pub fn paint_box(&self) -> Rect {
            let translation = self.geometry_state().border_box_translation();
            self.geometry_state().paint_box() - translation
        }

        /// Returns the widget's bounding-box rect in the window's coordinate space.
        ///
        /// It contains this widget and all of its descendents.
        ///
        /// This is the union of clipped effective paint-box rects, i.e. the union of
        /// globally transformed aligned border-box rects with paint insets applied.
        ///
        /// See [bounding box documentation] for more details.
        ///
        /// [bounding box documentation]: crate::doc::concepts#bounding-box
        pub fn bounding_box(&self) -> Rect {
            self.geometry_state().bounding_box()
        }

        /// Returns the first baseline relative to the top of the widget's aligned content-box.
        pub fn first_baseline(&self) -> f64 {
            self.geometry_state().first_baseline()
        }

        /// Returns the last baseline relative to the top of the widget's aligned content-box.
        pub fn last_baseline(&self) -> f64 {
            self.geometry_state().last_baseline()
        }

        /// The clip path of the widget, if any was set.
        ///
        /// The returned clip path will be in this widget's content-box coordinate space.
        ///
        /// For more information, see
        /// [`LayoutCtx::set_clip_path`](crate::core::LayoutCtx::set_clip_path).
        pub fn clip_path(&self) -> Option<Rect> {
            // Translate the clip path to the widget's content-box coordinate space.
            let translation = self.geometry_state().border_box_translation();
            self.geometry_state()
                .clip_path()
                .map(|path| path - translation)
        }

        /// Returns the [`Vec2`] for translating between this widget's
        /// content-box and border-box coordinate spaces.
        ///
        /// Add this [`Vec2`] to translate from content-box to border-box,
        /// and subtract this [`Vec2`] to translate from border-box to content-box.
        pub fn border_box_translation(&self) -> Vec2 {
            self.geometry_state().border_box_translation()
        }

        /// Returns the global transform mapping this widget's content-box coordinate space
        /// to the window's coordinate space.
        ///
        /// Computed from all `transform`, `scroll_translation`, and `origin` values
        /// from this widget all the way up to the window.
        ///
        /// Multiply by this to convert from this widget's content-box coordinate space to the window's,
        /// or use the inverse of this transform to go from window's space to this widget's content-box.
        pub fn window_transform(&self) -> Affine {
            let translation = self.geometry_state().border_box_translation();
            self.geometry_state()
                .window_transform()
                .pre_translate(translation)
        }

        /// Converts the `point` from the window's coordinate space
        /// to this widget's content-box coordinate space.
        pub fn to_local(&self, point: Point) -> Point {
            let to_border_box = self.geometry_state().window_transform().inverse();
            let to_content_box = -self.geometry_state().border_box_translation();
            to_border_box.then_translate(to_content_box) * point
        }

        /// Converts the `point` from this widget's content-box coordinate space
        /// to the window's coordinate space.
        ///
        /// The returned point is relative to the window's content area; it excludes window chrome.
        pub fn to_window(&self, point: Point) -> Point {
            let translation = self.geometry_state().border_box_translation();
            self.geometry_state().window_transform() * (point + translation)
        }

        /// Returns the DPI scaling factor.
        ///
        /// This can be useful for loading image resources meant for a specific scale.
        pub fn scale_factor(&self) -> f64 {
            self.geometry.scale(self.global_state.scale_factor)
        }
    }
);

// --- MARK: GET STATUS

// Methods on all context types
// Access status information (hovered/pointer captured/disabled/etc).
impl_context_method!(
    RawCtx<'_>,
    MutateCtx<'_>,
    ActionCtx<'_>,
    QueryCtx<'_>,
    EventCtx<'_>,
    UpdateCtx<'_>,
    MeasureCtx<'_>,
    LayoutCtx<'_>,
    ComposeCtx<'_>,
    PaintCtx<'_>,
    AccessCtx<'_>,
    {
        /// The "hovered" status of a widget.
        ///
        /// A widget is "hovered" when a pointer is hovering over its border-box.
        /// Widgets will often change their appearance as a visual indication that they
        /// will respond to pointer (usually mouse) interaction.
        ///
        /// If the pointer is [captured], then only the capturing widget can have hovered
        /// status. If the pointer is captured but not hovering over the captured
        /// widget, then no widget has the hovered status.
        ///
        /// [captured]: crate::doc::concepts#pointer-capture
        pub fn is_hovered(&self) -> bool {
            if self.global_state.dispatching_pointer {
                self.global_state.pointer.hovered_path.first() == Some(&self.widget_state.id)
            } else {
                self.widget_state.is_hovered
            }
        }

        /// Whether this widget or any of its descendants are hovered.
        ///
        /// To check if only this specific widget is hovered use [`is_hovered`](Self::is_hovered).
        pub fn has_hovered(&self) -> bool {
            if self.global_state.dispatching_pointer {
                self.global_state
                    .pointer
                    .hovered_path
                    .contains(&self.widget_state.id)
            } else {
                self.widget_state.has_hovered
            }
        }

        /// The "active" status of a widget.
        ///
        /// A widget is "active" when the user is "actively" interacting with it.
        /// Currently, a widget is determined to be active when it has [captured] a pointer,
        /// but this may change in the future to account for e.g. keyboard interactions.
        ///
        /// [captured]: crate::doc::concepts#pointer-capture
        pub fn is_active(&self) -> bool {
            if self.global_state.dispatching_pointer {
                self.global_state.pointer.pointer_capture_target == Some(self.widget_state.id)
            } else {
                self.widget_state.is_active
            }
        }

        /// Whether this widget or any of its descendants are active.
        ///
        /// To check if only this specific widget is active use [`is_active`](Self::is_active).
        pub fn has_active(&self) -> bool {
            if self.global_state.dispatching_pointer {
                self.global_state
                    .pointer
                    .active_path
                    .contains(&self.widget_state.id)
            } else {
                self.widget_state.has_active
            }
        }

        /// Whether a pointer is [captured] by this widget.
        ///
        /// During pointer dispatch this refers to the event's contact. Other
        /// contexts report whether any pointer is captured by this widget.
        ///
        /// [captured]: crate::doc::concepts#pointer-capture
        pub fn is_pointer_capture_target(&self) -> bool {
            self.global_state.pointer.pointer_capture_target == Some(self.widget_state.id)
                || (!self.global_state.dispatching_pointer
                    && self
                        .global_state
                        .other_pointers
                        .iter()
                        .any(|p| p.pointer_capture_target == Some(self.widget_state.id)))
        }

        /// The [text focus] status of a widget.
        ///
        /// The focused widget is the one that receives keyboard events.
        ///
        /// Returns `true` if this specific widget is focused.
        /// To check if any descendants are focused use [`has_focus_target`].
        ///
        /// [text focus]: crate::doc::concepts#text-focus
        /// [`has_focus_target`]: Self::has_focus_target
        pub fn is_focus_target(&self) -> bool {
            self.global_state.focused_widget == Some(self.widget_id())
        }

        /// Whether this widget or any of its descendants are focused.
        ///
        /// To check if only this specific widget is focused use [`is_focus_target`](Self::is_focus_target).
        pub fn has_focus_target(&self) -> bool {
            self.widget_state.has_focus_target
        }

        /// Whether keyboard/controller input currently requests focus indications.
        /// Combine with this widget's focus state when painting an indication.
        pub fn focus_visible(&self) -> bool {
            self.global_state.focus_visible
        }

        /// Whether the window is focused.
        pub fn is_window_focused(&self) -> bool {
            self.global_state.window_focused
        }

        /// The widget, if any, which has [pointer capture].
        ///
        /// During pointer dispatch this refers to the event's contact. Other
        /// contexts report whether any pointer is captured by this widget.
        ///
        /// [pointer capture]: crate::doc::concepts#pointer-capture
        pub fn pointer_capture_target_id(&self) -> Option<WidgetId> {
            self.global_state.pointer.pointer_capture_target
        }

        /// The widget, if any, which has [text focus].
        ///
        /// The focused widget is the one that receives keyboard events.
        ///
        /// [text focus]: crate::doc::concepts#text-focus
        pub fn focus_target_id(&self) -> Option<WidgetId> {
            self.global_state.focused_widget
        }

        /// Whether the focused widget owns text keyboard input, including read-only editors.
        pub fn is_text_input_focused(&self) -> bool {
            self.global_state.is_text_input_focused
        }

        /// Whether this widget gets pointer events and hovered status.
        ///
        /// See [`Widget::accepts_pointer_interaction()`] for details.
        pub fn accepts_pointer_interaction(&self) -> bool {
            self.geometry_state().accepts_pointer_interaction()
        }

        /// Whether children of this widget get pointer events and hovered status.
        ///
        /// See [`Widget::propagates_pointer_interaction()`] for details.
        pub fn propagates_pointer_interaction(&self) -> bool {
            self.geometry_state().propagates_pointer_interaction()
        }

        /// Whether this widget gets text focus.
        ///
        /// See [`Widget::accepts_focus()`] for details.
        pub fn accepts_focus(&self) -> bool {
            self.widget_state.accepts_focus
        }

        /// Whether this widget gets IME events.
        ///
        /// See [`Widget::accepts_text_input()`] for details.
        pub fn accepts_text_input(&self) -> bool {
            self.widget_state.accepts_text_input
        }

        /// Whether this widget is [disabled].
        ///
        /// Returns `true` if this widget or any of its ancestors is explicitly disabled.
        /// To make this widget explicitly disabled use [`set_disabled`].
        ///
        /// [disabled]: crate::doc::concepts#disabled
        /// [`set_disabled`]: EventCtx::set_disabled
        pub fn is_disabled(&self) -> bool {
            self.widget_state.is_disabled
        }

        /// Whether this widget is [stashed].
        ///
        /// [stashed]: crate::doc::concepts#stashed
        pub fn is_stashed(&self) -> bool {
            self.widget_state.is_stashed
        }

        /// Returns whether the given child is [stashed].
        ///
        /// The result of this function is not affected by `set_stashed` until the next
        /// [`update_stashed`] pass.
        ///
        /// [stashed]: crate::doc::concepts#stashed
        /// [`update_stashed`]: crate::doc::pass_system#update-passes
        pub fn child_is_stashed(&self, child: &WidgetPod<impl Widget + ?Sized>) -> Option<bool> {
            self.geometry.child(child.id())?;
            self.children
                .item(child.id())
                .map(|node| node.item.state.is_stashed)
        }
    }
);

mod mutation;
