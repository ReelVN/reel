// Copyright 2024 the Xilem Authors
// SPDX-License-Identifier: Apache-2.0

use tracing::{info_span, trace};

use crate::app::{GeometryView, RenderRoot, RenderRootSignal};
use crate::core::keyboard::{Key, KeyState, NamedKey};
use crate::core::{
    AccessEvent, EventCtx, Handled, Ime, PointerButtonEvent, PointerEvent, PointerGestureEvent,
    PointerScrollEvent, PointerType, PointerUpdate, PropertiesMut, TextEvent, Widget, WidgetId,
};
use crate::dpi::{LogicalPosition, PhysicalPosition};
use crate::passes::update::find_next_focusable;
use crate::passes::{enter_span, merge_state_up};

// --- MARK: HELPERS
fn get_pointer_target(
    root: &RenderRoot,
    pointer_pos: Option<LogicalPosition<f64>>,
    view: GeometryView<'_>,
) -> Option<WidgetId> {
    // See the [pointer capture documentation](../doc/concepts.md#pointer-capture).
    if let Some(capture_target) = root.global_state.pointer.pointer_capture_target
        && root.widget_arena.has(capture_target)
    {
        return Some(capture_target);
    }

    if let Some(pointer_pos) = pointer_pos {
        let pointer_pos = (pointer_pos.x, pointer_pos.y).into();
        return root.pointer_hit_test(pointer_pos, view);
    }

    None
}

/// `true` if this [`PointerEvent`] type is likely to occur every frame.
fn is_very_frequent(e: &PointerEvent) -> bool {
    matches!(e, PointerEvent::Move(..) | PointerEvent::Scroll { .. })
}

/// Short name for a [`PointerEvent`].
fn pointer_event_short_name(e: &PointerEvent) -> &'static str {
    match e {
        PointerEvent::Down(..) => "Down",
        PointerEvent::Up(..) => "Up",
        PointerEvent::Move(..) => "Move",
        PointerEvent::Enter(..) => "Enter",
        PointerEvent::Leave(..) => "Leave",
        PointerEvent::Cancel(..) => "Cancel",
        PointerEvent::Scroll(..) => "Scroll",
        PointerEvent::Gesture(..) => "Gesture",
    }
}

/// A position if the event has one.
fn try_event_position(event: &PointerEvent) -> Option<PhysicalPosition<f64>> {
    match event {
        PointerEvent::Down(PointerButtonEvent { state, .. })
        | PointerEvent::Up(PointerButtonEvent { state, .. })
        | PointerEvent::Move(PointerUpdate { current: state, .. })
        | PointerEvent::Scroll(PointerScrollEvent { state, .. })
        | PointerEvent::Gesture(PointerGestureEvent { state, .. }) => Some(state.position),
        _ => None,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "The pass binds one event to its selected geometry, route and eligibility policy"
)]
pub(super) fn run_event_pass<E>(
    root: &mut RenderRoot,
    target: Option<WidgetId>,
    event: &E,
    skip_if_disabled: bool,
    allow_pointer_capture: bool,
    pass_fn: impl FnMut(&mut dyn Widget, &mut EventCtx<'_>, &mut PropertiesMut<'_>, &E),
    trace: bool,
    view: GeometryView<'_>,
    route: Option<&[WidgetId]>,
) -> Handled {
    let mut pass_fn = pass_fn;

    if let Some(id) = target
        && !root.has_widget(id)
    {
        debug_panic!("Cannot send event to non-existent widget {id}.");
        return Handled::No;
    }

    let original_target = target;
    // Cancellation and IME teardown still reach their original owner.
    let scope = skip_if_disabled.then(|| root.input_scope(view)).flatten();
    let mut outside_scope =
        skip_if_disabled && target.is_some_and(|id| !root.input_in_scope(id, view));
    let owns_route = route.is_none();
    let mut buffer = if owns_route {
        std::mem::take(&mut root.global_state.pointer_route)
    } else {
        Vec::new()
    };
    if owns_route {
        buffer.clear();
        if let Some(id) = target {
            let _ = root.input_path_into(id, view, &mut buffer);
        }
    }
    let mut is_handled = false;
    for &widget_id in route.unwrap_or(&buffer) {
        let Some(geometry) = view.bind(widget_id) else {
            continue;
        };
        if !root.has_widget(widget_id) {
            continue;
        }
        let mut node = root.widget_arena.get_node_mut(widget_id);

        if skip_if_disabled && (node.item.state.is_disabled || node.item.state.is_stashed) {
            // Ineligible widgets still delimit the accepted modal scope.
            outside_scope |= scope == Some(widget_id);
            continue;
        }

        if !is_handled && !outside_scope {
            let _span = enter_span(&node.item.state);
            let widget_type_id = node.item.widget.type_id();
            let stack = root
                .property_arena
                .get(node.item.state.property_stack_id, widget_type_id);
            let mut ctx = EventCtx {
                geometry,
                global_state: &mut root.global_state,
                widget_state: &mut node.item.state,
                children: node.children.reborrow_mut(),
                property_arena: &root.property_arena,
                target: original_target.unwrap(),
                allow_pointer_capture,
                is_handled: false,
            };
            let widget = &mut *node.item.widget;
            if trace {
                trace!(
                    "Widget '{}' {} visited",
                    widget.short_type_name(),
                    widget_id,
                );
            }

            let mut props = PropertiesMut {
                local: &mut node.item.properties,
                default_map: root
                    .property_arena
                    .default_properties
                    .for_widget(widget.type_id()),
                stack,
                class_set: &node.item.class_set,
            };
            pass_fn(widget, &mut ctx, &mut props, event);
            is_handled = ctx.is_handled;
        }

        merge_state_up(&mut root.widget_arena, widget_id);
        outside_scope |= scope == Some(widget_id);
    }

    if owns_route {
        buffer.clear();
        root.global_state.pointer_route = buffer;
    }
    Handled::from(is_handled)
}

// --- MARK: POINTER_EVENT
pub(crate) fn set_focus_visible(root: &mut RenderRoot, visible: bool) {
    if root.global_state.focus_visible != visible {
        root.global_state.focus_visible = visible;
        super::update::run_global_update_pass(
            root,
            &crate::core::Update::FocusVisibilityChanged(visible),
        );
    }
}

/// See the [passes documentation](crate::doc::pass_system#event-passes).
pub(crate) fn run_on_pointer_event_with_view(
    root: &mut RenderRoot,
    event: &PointerEvent,
    view: GeometryView<'_>,
) -> Handled {
    let _span = info_span!("dispatch_pointer_event").entered();

    trace!(
        "Running ON_POINTER_EVENT pass with {}",
        pointer_event_short_name(event)
    );

    let scale = view.scale(root.global_state.scale_factor);
    if matches!(event, PointerEvent::Down(_) | PointerEvent::Scroll(_)) {
        super::navigation::cancel_virtual(root);
    }
    let physical_pos = try_event_position(event);
    if root.global_state.pointer.info.pointer_type == PointerType::Mouse {
        if !matches!(event, PointerEvent::Cancel(_)) {
            root.global_state.cursor.mouse_active = physical_pos.is_some();
        }
    } else if matches!(event, PointerEvent::Down(_) | PointerEvent::Move(_)) {
        root.global_state.cursor.mouse_active = false;
    }
    if matches!(event, PointerEvent::Down(_) | PointerEvent::Scroll(_))
        || matches!(event, PointerEvent::Move(_))
            && physical_pos != root.global_state.pointer.last_pointer_physical
    {
        set_focus_visible(root, false);
    }
    root.global_state.pointer.last_pointer_physical = physical_pos;
    let event_pos = physical_pos.map(|p| p.to_logical(scale));

    if event_pos != root.global_state.pointer.last_mouse_pos {
        root.global_state.pointer.last_mouse_pos = event_pos;
    }
    root.global_state.needs_pointer_pass = true;
    if physical_pos.is_some() {
        crate::passes::update::run_update_pointer_with_view(root, view);
    }

    if root.global_state.inspector_state.is_picking_widget
        && matches!(event, PointerEvent::Move(..))
    {
        return Handled::Yes;
    }

    // If the widget picker is active and this is a click event,
    // we select the widget under the mouse and short-circuit the event pass.
    if root.global_state.inspector_state.is_picking_widget
        && matches!(event, PointerEvent::Down { .. })
    {
        let target_widget_id = get_pointer_target(root, event_pos, view);
        if let Some(target_widget_id) = target_widget_id {
            root.global_state
                .emit_signal(RenderRootSignal::WidgetSelectedInInspector(
                    target_widget_id,
                ));
        }
        root.global_state.inspector_state.is_picking_widget = false;
        root.global_state.inspector_state.hovered_widget = None;
        root.root_state_mut().needs_paint = true;
        return Handled::Yes;
    }

    let layer_ids: smallvec::SmallVec<[WidgetId; 16]> = match view {
        GeometryView::Live => root
            .widget_arena
            .get_node(root.root_id())
            .item
            .widget
            .children_ids()
            .into_iter()
            .collect(),
        GeometryView::Presented(Some(v)) => v.child_ids(0).collect(),
        GeometryView::Presented(None) => smallvec::SmallVec::new(),
    };
    for layer_id in layer_ids {
        // Layer interception is input behavior, not terminal capture cleanup.
        // Cancel for the captured widget is dispatched separately below.
        if !root.is_still_interactive(layer_id) || !root.input_in_scope(layer_id, view) {
            continue;
        }
        let Some(geometry) = view.bind_visible(layer_id) else {
            continue;
        };
        let mut layer_root = root.widget_arena.get_node_mut(layer_id);
        let layer_type_id = layer_root.item.widget.type_id();
        if let Some(layer) = layer_root.item.widget.as_layer() {
            let stack = root
                .property_arena
                .get(layer_root.item.state.property_stack_id, layer_type_id);
            let mut ctx = EventCtx {
                geometry,
                global_state: &mut root.global_state,
                widget_state: &mut layer_root.item.state,
                children: layer_root.children.reborrow_mut(),
                property_arena: &root.property_arena,
                target: layer_id,
                allow_pointer_capture: false,
                is_handled: false,
            };
            let mut props = PropertiesMut {
                local: &mut layer_root.item.properties,
                default_map: root
                    .property_arena
                    .default_properties
                    .for_widget(layer.type_id()),
                stack,
                class_set: &layer_root.item.class_set,
            };

            ctx.global_state.dispatching_pointer = true;
            layer.capture_pointer_event(&mut ctx, &mut props, event);
            ctx.global_state.dispatching_pointer = false;
        }
    }

    let mut target_widget_id = get_pointer_target(root, event_pos, view);

    let mut route = std::mem::take(&mut root.global_state.pointer_route);
    route.clear();
    if let Some(id) = target_widget_id {
        let _ = root.input_path_into(id, view, &mut route);
    }
    if !route.is_empty()
        && matches!(event, PointerEvent::Down { .. })
        && let Some(target_widget_id) = target_widget_id
    {
        // The next tab event will assign focus around this widget.
        root.global_state.focus_anchor = Some(target_widget_id);

        // If we click outside of the focused widget, we clear the focus.
        if let Some(focused_widget) = root.global_state.focused_widget {
            // Focused_widget isn't ancestor of target_widget_id
            if !route.contains(&focused_widget) {
                root.global_state.next_focused_widget = None;
            }
        }
    }

    // Pan recognition uses the same accepted ancestor path and storage as bubbling.
    // Exclusive drag captures (e.g. sliders) do not participate in takeover.
    if matches!(event, PointerEvent::Down(_)) {
        root.global_state.pointer.pan_target = target_widget_id;
    }
    let old_capture = root.global_state.pointer.pointer_capture_target;
    if root.global_state.pointer.info.pointer_type == PointerType::Touch
        && (!matches!(event, PointerEvent::Move(_))
            || old_capture.is_none()
            || root.global_state.pointer.pointer_capture_allows_pan)
    {
        let preview_target = root
            .global_state
            .pointer
            .pan_target
            .filter(|id| root.has_widget(*id))
            .or(target_widget_id);
        if preview_target != target_widget_id {
            route.clear();
            if let Some(id) = preview_target {
                let _ = root.input_path_into(id, view, &mut route);
            }
        }
        run_event_pass(
            root,
            preview_target,
            event,
            !matches!(event, PointerEvent::Cancel(_)),
            matches!(event, PointerEvent::Move(_)),
            |widget, ctx, props, event| {
                ctx.global_state.dispatching_pointer = true;
                widget.on_pointer_preview(ctx, props, event);
                ctx.global_state.dispatching_pointer = false;
            },
            false,
            view,
            Some(&route),
        );
        let captured = root.global_state.pointer.pointer_capture_target;
        if captured != old_capture
            && let Some(new_target) = captured
        {
            let revision = root.global_state.pointer.pointer_capture_revision;
            if let Some(old) = old_capture {
                root.global_state.pointer.pointer_capture_target = Some(old);
                let cancel = PointerEvent::Cancel(root.global_state.pointer.info);
                run_event_pass(
                    root,
                    Some(old),
                    &cancel,
                    false,
                    false,
                    |widget, ctx, props, event| {
                        ctx.global_state.dispatching_pointer = true;
                        widget.on_pointer_event(ctx, props, event);
                        ctx.global_state.dispatching_pointer = false;
                    },
                    false,
                    view,
                    Some(&[old]),
                );
            }
            root.global_state.pointer.pointer_capture_target = Some(new_target);
            root.global_state.pointer.pointer_capture_revision = revision;
            root.global_state.pointer.pointer_capture_allows_pan = false;
            root.global_state.pointer.pointer_capture_terminal_delivered = None;
            target_widget_id = Some(new_target);
        }
        if preview_target != target_widget_id {
            route.clear();
            if let Some(id) = target_widget_id {
                let _ = root.input_path_into(id, view, &mut route);
            }
        }
    }

    let skip_if_disabled = !matches!(event, PointerEvent::Cancel { .. });
    let handled = run_event_pass(
        root,
        target_widget_id,
        event,
        skip_if_disabled,
        matches!(event, PointerEvent::Down { .. }),
        |widget, ctx, props, event| {
            // Ordinary controls have one gesture owner. A second contact can
            // operate another control but cannot steal an in-progress drag.
            if matches!(event, PointerEvent::Down(_))
                && !widget.accepts_multiple_pointers()
                && ctx
                    .global_state
                    .other_pointers
                    .iter()
                    .any(|p| p.pointer_capture_target == Some(ctx.widget_state.id))
            {
                ctx.set_handled();
                return;
            }
            if matches!(event, PointerEvent::Up(_) | PointerEvent::Cancel(_))
                && ctx.global_state.pointer.pointer_capture_target == Some(ctx.widget_state.id)
            {
                ctx.global_state.pointer.pointer_capture_terminal_delivered =
                    Some(ctx.widget_state.id);
            }
            ctx.global_state.dispatching_pointer = true;
            widget.on_pointer_event(ctx, props, event);
            ctx.global_state.dispatching_pointer = false;
        },
        !is_very_frequent(event),
        view,
        Some(&route),
    );
    route.clear();
    root.global_state.pointer_route = route;

    let retaining_buttons = matches!(event, PointerEvent::Up(e) if !e.state.buttons.is_empty())
        && root
            .global_state
            .pointer
            .pointer_capture_target
            .is_some_and(|id| {
                root.widget_arena
                    .nodes
                    .find(id)
                    .is_some_and(|n| n.item.widget.accepts_multiple_pointers())
            });
    if retaining_buttons {
        root.global_state.pointer.pointer_capture_terminal_delivered = None;
    }
    if matches!(event, PointerEvent::Up { .. } | PointerEvent::Cancel(..)) && !retaining_buttons {
        // Automatically release the pointer on pointer up or leave. If a widget holds the capture,
        // it is notified of the pointer event before the capture is released, so it knows it is
        // about to lose the pointer.
        root.global_state.pointer.pointer_capture_target = None;
        root.global_state.pointer.pan_target = None;
        root.global_state.pointer.pointer_capture_terminal_delivered = None;
        if root.global_state.pointer.info.pointer_type == PointerType::Touch {
            root.global_state.pointer.last_pointer_physical = None;
            root.global_state.pointer.last_mouse_pos = None;
        }
        root.global_state.needs_pointer_pass = true;
    }

    if !is_very_frequent(event) {
        trace!(
            focused_widget = root.global_state.focused_widget.map(|id| id.0),
            handled = handled.is_handled(),
            "ON_POINTER_EVENT finished",
        );
    }

    if root.input_scope(view).is_some() || matches!(view, GeometryView::Presented(None)) {
        Handled::Yes
    } else {
        handled
    }
}

// --- MARK: TEXT EVENT
/// See the [passes documentation](crate::doc::pass_system#event-passes).
pub(crate) fn run_on_text_event_pass(root: &mut RenderRoot, event: &TextEvent) -> Handled {
    if root.global_state.deferred_navigation.is_some()
        && matches!(event, TextEvent::Keyboard(key) if key.key == Key::Named(NamedKey::Enter)
            || matches!(&key.key, Key::Character(text) if text == " "))
    {
        return Handled::Yes;
    }
    if matches!(event, TextEvent::Keyboard(key) if key.state == KeyState::Down
        && !matches!(key.key, Key::Named(NamedKey::Shift | NamedKey::Control | NamedKey::Alt | NamedKey::Meta)))
    {
        set_focus_visible(root, true);
    }
    if let TextEvent::Navigation(event) = event {
        return super::navigation::run(root, *event);
    }
    if matches!(event, TextEvent::WindowFocusChange(false)) {
        super::navigation::cancel_all(root);
        root.cancel_pointer_inputs();
    }

    let _span = info_span!("dispatch_text_event").entered();

    trace!("Running ON_TEXT_EVENT pass with {}", event.short_name());

    if let TextEvent::WindowFocusChange(focused) = event
        && root.global_state.window_focused != *focused
    {
        root.global_state.window_focused = *focused;
        super::update::run_global_update_pass(
            root,
            &crate::core::Update::WindowFocusChanged(*focused),
        );
    }

    let cleanup = matches!(
        event,
        TextEvent::Ime(Ime::Disabled)
            | TextEvent::WindowFocusChange(false)
            | TextEvent::Input(crate::input::InputEvent::Cancel)
            | TextEvent::Input(crate::input::InputEvent::Disconnected(_))
    );
    let source = root.global_state.input_geometry_source.clone();
    let view = if cleanup {
        GeometryView::Live
    } else {
        source.borrow()
    };
    if matches!(view, GeometryView::Presented(None)) {
        return Handled::Yes;
    }

    let target = root
        .global_state
        .focused_widget
        .filter(|&id| cleanup || root.input_in_scope(id, view))
        .or_else(|| {
            if let Some(focus_fallback) = root.global_state.focus_fallback
                && root.is_still_interactive(focus_fallback)
                && (cleanup || root.input_in_scope(focus_fallback, view))
            {
                Some(focus_fallback)
            } else {
                None
            }
        })
        .or(root.input_scope(view).filter(|&id| root.has_widget(id)));

    // Like editors and sliders, a focused selection control gets first refusal
    // before an ancestor's raw keyboard callback or action binding.
    if let TextEvent::Keyboard(key) = event
        && target
            .and_then(|id| super::selection::member(root, id))
            .is_some()
    {
        use crate::input::{ActionPhase, NavigationAction as N, NavigationEvent};
        let direction = match key.key {
            Key::Named(NamedKey::ArrowLeft) => Some(N::Left),
            Key::Named(NamedKey::ArrowRight) => Some(N::Right),
            Key::Named(NamedKey::ArrowUp) => Some(N::Up),
            Key::Named(NamedKey::ArrowDown) => Some(N::Down),
            _ => None,
        };
        if let Some(action) = direction {
            super::navigation::run(
                root,
                NavigationEvent {
                    action,
                    value: 1.,
                    phase: if key.state.is_up() {
                        ActionPhase::Released
                    } else if key.repeat {
                        ActionPhase::Repeated
                    } else {
                        ActionPhase::Pressed
                    },
                },
            );
            return Handled::Yes;
        }
    }

    let skip_if_disabled = !cleanup;
    let mut handled = run_event_pass(
        root,
        target,
        event,
        skip_if_disabled,
        false,
        |widget, ctx, props, event| {
            widget.on_text_event(ctx, props, event);
        },
        true,
        view,
        None,
    );

    if let TextEvent::Keyboard(key) = event {
        if handled == Handled::No {
            use crate::input::{ActionPhase, NavigationAction as N, NavigationEvent};
            let action = match key.key {
                Key::Named(NamedKey::ArrowLeft) => Some(N::Left),
                Key::Named(NamedKey::ArrowRight) => Some(N::Right),
                Key::Named(NamedKey::ArrowUp) => Some(N::Up),
                Key::Named(NamedKey::ArrowDown) => Some(N::Down),
                Key::Named(NamedKey::Escape) => Some(N::Back),
                _ => None,
            };
            if let Some(action) = action {
                handled = super::navigation::run(
                    root,
                    NavigationEvent {
                        action,
                        value: 1.,
                        phase: if key.state.is_up() {
                            ActionPhase::Released
                        } else if key.repeat {
                            ActionPhase::Repeated
                        } else {
                            ActionPhase::Pressed
                        },
                    },
                );
            }
        }
        // Handle Tab focus
        if key.key == Key::Named(NamedKey::Tab)
            && key.state == KeyState::Down
            && handled == Handled::No
        {
            let forward = !key.modifiers.shift();
            let leaving_group = root
                .global_state
                .focused_widget
                .and_then(|id| super::selection::member(root, id))
                .is_some();
            let next_focused_widget = find_next_focusable(root, forward);
            if leaving_group
                || !super::navigation::defer_virtual(root, next_focused_widget, forward, None)
            {
                if leaving_group {
                    super::navigation::cancel_virtual(root);
                }
                root.global_state.next_focused_widget = next_focused_widget;
            }
            handled = Handled::Yes;
        }

        if key.key == Key::Named(NamedKey::F11)
            && key.state == KeyState::Down
            && handled == Handled::No
        {
            root.global_state.inspector_state.is_picking_widget =
                !root.global_state.inspector_state.is_picking_widget;
            root.global_state.inspector_state.hovered_widget = None;
            root.root_state_mut().needs_paint = true;
            handled = Handled::Yes;
        }

        if key.key == Key::Named(NamedKey::F12)
            && key.state == KeyState::Down
            && handled == Handled::No
        {
            root.global_state.debug_paint = !root.global_state.debug_paint;
            root.root_state_mut().needs_paint = true;
            handled = Handled::Yes;
        }
    }

    trace!(
        focused_widget = root.global_state.focused_widget.map(|id| id.0),
        handled = handled.is_handled(),
        "ON_TEXT_EVENT finished",
    );

    // Physical controls report whether an action claimed them. The host may
    // route unbound controls into semantic navigation within this same scope.
    if root.input_scope(view).is_some() && !matches!(event, TextEvent::Input(_)) {
        Handled::Yes
    } else {
        handled
    }
}

// --- MARK: ACCESS EVENT
/// See the [passes documentation](crate::doc::pass_system#event-passes).
pub(crate) fn run_on_access_event_pass(
    root: &mut RenderRoot,
    event: &AccessEvent,
    target: WidgetId,
) -> Handled {
    let _span = info_span!("access_event").entered();
    trace!("Running ON_ACCESS_EVENT pass with {}", event.short_name());

    let source = root.global_state.input_geometry_source.clone();
    let view = source.borrow();
    if !root.input_in_scope(target, view) {
        return Handled::Yes;
    }

    let skip_if_disabled = true;
    let mut handled = run_event_pass(
        root,
        Some(target),
        event,
        skip_if_disabled,
        false,
        |widget, ctx, props, event| {
            widget.on_access_event(ctx, props, event);
        },
        true,
        view,
        None,
    );

    // Handle focus events
    match event.action {
        accesskit::Action::Focus if !handled.is_handled() && root.focus_in_scope(target) => {
            root.global_state.next_focused_widget = Some(target);
            handled = Handled::Yes;
        }
        accesskit::Action::Blur
            if !handled.is_handled() && root.global_state.next_focused_widget == Some(target) =>
        {
            root.global_state.next_focused_widget = None;
            handled = Handled::Yes;
        }
        accesskit::Action::ScrollIntoView if !handled.is_handled() => {
            let widget_state = root.widget_arena.get_state(target);
            let rect = widget_state.border_box();
            root.global_state
                .scroll_request_targets
                .push((target, rect));
            handled = Handled::Yes;
        }
        _ => {}
    }

    trace!(
        focused_widget = root.global_state.focused_widget.map(|id| id.0),
        handled = handled.is_handled(),
        "ON_ACCESS_EVENT finished",
    );

    if root.input_scope(view).is_some() {
        Handled::Yes
    } else {
        handled
    }
}
