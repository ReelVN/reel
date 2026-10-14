// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

use super::{
    event::run_event_pass,
    update::{find_first_focusable, run_update_focus_pass},
};
use crate::app::{GeometryView, RenderRoot};
use crate::core::{Handled, TextEvent, WidgetId};
use crate::input::{ActionPhase, NavigationAction, NavigationEvent};

fn target(root: &RenderRoot, view: GeometryView<'_>) -> Option<WidgetId> {
    root.global_state
        .focused_widget
        .or(root.global_state.focus_fallback)
        .filter(|&id| root.is_still_interactive(id) && root.input_in_scope(id, view))
        .or(root.input_scope(view))
}
fn dispatch(
    root: &mut RenderRoot,
    target: Option<WidgetId>,
    event: NavigationEvent,
    view: GeometryView<'_>,
) -> Handled {
    run_event_pass(
        root,
        target,
        &TextEvent::Navigation(event),
        event.phase != ActionPhase::Cancelled,
        false,
        |w, c, p, e| w.on_text_event(c, p, e),
        true,
        view,
        None,
    )
}
pub(crate) fn cancel_all(root: &mut RenderRoot) {
    root.global_state.navigation_directions.clear();
    cancel(root);
}
pub(crate) fn cancel(root: &mut RenderRoot) {
    if let Some(id) = root.global_state.navigation_target.take()
        && root.has_widget(id)
    {
        let source = root.global_state.input_geometry_source.clone();
        dispatch(
            root,
            Some(id),
            NavigationEvent {
                action: NavigationAction::Confirm,
                phase: ActionPhase::Cancelled,
                value: 0.,
            },
            source.borrow(),
        );
    }
}
pub(crate) fn run(root: &mut RenderRoot, event: NavigationEvent) -> Handled {
    let source = root.global_state.input_geometry_source.clone();
    let view = source.borrow();
    if matches!(view, GeometryView::Presented(None)) {
        return Handled::Yes;
    }
    if event.action.is_direction() {
        match event.phase {
            ActionPhase::Pressed => {
                if !root
                    .global_state
                    .navigation_directions
                    .contains(&event.action)
                {
                    root.global_state.navigation_directions.push(event.action);
                }
            }
            ActionPhase::Released | ActionPhase::Cancelled => root
                .global_state
                .navigation_directions
                .retain(|a| *a != event.action),
            ActionPhase::Repeated
                if !root
                    .global_state
                    .navigation_directions
                    .contains(&event.action) =>
            {
                return Handled::Yes;
            }
            _ => {}
        }
    }
    if event.action == NavigationAction::Confirm && event.phase == ActionPhase::Pressed {
        // Enter a menu without requiring a pointer. A dialogue with no focusable
        // controls uses its existing explicit fallback.
        if root.global_state.focused_widget.is_none() {
            let scope = root.input_scope(view).unwrap_or(root.root_id());
            root.global_state.next_focused_widget = find_first_focusable(root, &[], scope, true);
            run_update_focus_pass(root);
        }
        cancel(root);
        root.global_state.navigation_target = target(root, view);
    }
    let selected = if event.action == NavigationAction::Confirm {
        match event.phase {
            ActionPhase::Released | ActionPhase::Cancelled => {
                let owner = root.global_state.navigation_target.take();
                owner.filter(|&id| {
                    root.is_still_interactive(id)
                        && root.input_in_scope(id, view)
                        && target(root, view) == Some(id)
                })
            }
            ActionPhase::Pressed => root.global_state.navigation_target,
            _ => return Handled::Yes,
        }
    } else {
        target(root, view)
    };
    let mut handled = dispatch(root, selected, event, view);
    if !handled.is_handled()
        && event.action.is_direction()
        && matches!(event.phase, ActionPhase::Pressed | ActionPhase::Repeated)
    {
        let next = directional_focus(root, event.action, view);
        if next.is_some() {
            root.global_state.next_focused_widget = next;
        }
        handled = Handled::Yes;
    }
    if root.input_scope(view).is_some() || event.action == NavigationAction::Confirm {
        Handled::Yes
    } else {
        handled
    }
}

fn directional_focus(
    root: &mut RenderRoot,
    direction: NavigationAction,
    view: GeometryView<'_>,
) -> Option<WidgetId> {
    let scope = root.input_scope(view).unwrap_or(root.root_id());
    let Some(focus) = root.global_state.focused_widget else {
        return find_first_focusable(root, &[], scope, true);
    };
    let center = |root: &RenderRoot, id| {
        let state = &root.widget_arena.nodes.find(id)?.item.state;
        let geometry = view.bind_visible(id)?.resolve(state);
        Some(geometry.window_transform() * geometry.border_box().center())
    };
    let origin = center(root, focus)?;
    let mut walk = std::mem::take(&mut root.global_state.navigation_walk);
    let mut best: Option<(f64, WidgetId)> = None;
    let mut consider = |id| {
        let Some(node) = root.widget_arena.nodes.find(id) else {
            return;
        };
        if id == focus || !node.item.state.accepts_focus || !root.focus_in_scope(id) {
            return;
        }
        let Some(position) = center(root, id) else {
            return;
        };
        let delta = position - origin;
        let (ahead, across) = match direction {
            NavigationAction::Left => (-delta.x, delta.y),
            NavigationAction::Right => (delta.x, delta.y),
            NavigationAction::Up => (-delta.y, delta.x),
            NavigationAction::Down => (delta.y, delta.x),
            _ => unreachable!(),
        };
        if ahead <= 0. {
            return;
        }
        let distance = ahead * ahead + 4. * across * across;
        if best.is_none_or(|(old, _)| distance < old) {
            best = Some((distance, id));
        }
    };
    if let GeometryView::Presented(Some(view)) = view {
        // The accepted flat geometry already supplies painter order and positions.
        // No new per-navigation tree projection or child list allocation.
        for id in view.navigation_ids() {
            consider(id);
        }
    } else {
        walk.clear();
        walk.push(scope);
        while let Some(id) = walk.pop() {
            let Some(node) = root.widget_arena.nodes.find(id) else {
                continue;
            };
            if !node.item.state.descendant_is_focusable
                || node.item.state.is_disabled
                || node.item.state.is_stashed
            {
                continue;
            }
            walk.extend(node.item.widget.children_ids().into_iter().rev());
            consider(id);
        }
    }
    root.global_state.navigation_walk = walk;
    best.map(|(_, id)| id)
}
