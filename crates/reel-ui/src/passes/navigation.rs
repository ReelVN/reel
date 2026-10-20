// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

use super::{
    event::run_event_pass,
    update::{find_first_focusable, run_single_update_pass, run_update_focus_pass},
};
use crate::app::{GeometryView, RenderRoot};
use crate::core::{Handled, SelectionGroupId, TextEvent, WidgetId};
use crate::input::{ActionPhase, NavigationAction, NavigationEvent};

#[derive(Clone, Copy)]
pub(crate) struct DeferredNavigation {
    serial: u64,
    revision: u64,
    owner: WidgetId,
    anchor: Option<WidgetId>,
    index: usize,
    forward: bool,
    direction: Option<NavigationAction>,
    selection: Option<SelectionGroupId>,
}

pub(super) fn select_deferred_member(root: &mut RenderRoot, group: SelectionGroupId) {
    if let Some(request) = &mut root.global_state.deferred_navigation {
        request.selection = Some(group);
    }
}

fn request_virtual(root: &mut RenderRoot, mut request: DeferredNavigation) {
    root.global_state.virtual_navigation_serial += 1;
    request.serial = root.global_state.virtual_navigation_serial;
    request.revision = root
        .widget_arena
        .get_node(request.owner)
        .item
        .widget
        .virtual_children()
        .unwrap()
        .revision;
    cancel(root);
    root.global_state.deferred_navigation = Some(request);
    run_single_update_pass(root, Some(request.owner), |widget, ctx, props| {
        widget.request_virtual_child(
            ctx,
            props,
            Some((request.serial, request.index, request.forward)),
        );
    });
}

pub(crate) fn cancel_virtual(root: &mut RenderRoot) {
    if let Some(request) = root.global_state.deferred_navigation.take() {
        run_single_update_pass(root, Some(request.owner), |widget, ctx, props| {
            widget.request_virtual_child(ctx, props, None);
        });
    }
}

pub(super) fn follows_in_tree(root: &RenderRoot, anchor: WidgetId, candidate: WidgetId) -> bool {
    if root.is_descendant(candidate, anchor) {
        return true;
    }
    if root.is_descendant(anchor, candidate) {
        return false;
    }
    let mut current = anchor;
    while let Some(parent) = root.widget_arena.parent_of(current) {
        if root.is_descendant(candidate, parent) {
            let mut other = candidate;
            while root.widget_arena.parent_of(other) != Some(parent) {
                other = root.widget_arena.parent_of(other).expect("common ancestor");
            }
            let children = root
                .widget_arena
                .get_node(parent)
                .item
                .widget
                .children_ids();
            return children.iter().position(|&id| id == other)
                > children.iter().position(|&id| id == current);
        }
        current = parent;
    }
    false
}

/// A group's logical boundary is not the first/last row of the mounted window.
/// Entering another list or wrapping requests its actual endpoint through the
/// same measured, accepted-frame navigation protocol as an ordinary row edge.
pub(super) fn defer_group_entry(
    root: &mut RenderRoot,
    candidate: WidgetId,
    group: SelectionGroupId,
    forward: bool,
    direction: NavigationAction,
    wrapping: bool,
) -> bool {
    let anchor = root.global_state.focused_widget;
    let mut row = candidate;
    let mut target = None;
    while let Some(owner) = root.widget_arena.parent_of(row) {
        if let Some(list) = root
            .widget_arena
            .get_node(owner)
            .item
            .widget
            .virtual_children()
            && (wrapping || anchor.is_none_or(|anchor| !root.is_descendant(anchor, owner)))
        {
            if let crate::app::InputGeometrySource::Presented(view) =
                &root.global_state.input_geometry_source
                && view
                    .as_ref()
                    .and_then(|view| view.get(owner))
                    .and_then(|record| record.virtual_revision)
                    != Some(list.revision)
            {
                return true;
            }
            if list.total > 0 {
                target = Some((owner, if forward { 0 } else { list.total - 1 }));
            }
        }
        row = owner;
    }
    let Some((owner, index)) = target else {
        return false;
    };
    request_virtual(
        root,
        DeferredNavigation {
            serial: 0,
            revision: 0,
            owner,
            anchor,
            index,
            forward,
            direction: Some(direction),
            selection: Some(group),
        },
    );
    true
}

fn entry_gap(
    root: &mut RenderRoot,
    candidate: Option<WidgetId>,
    forward: bool,
) -> Option<(WidgetId, usize)> {
    let inspect = |root: &RenderRoot, owner| {
        if !root.focus_in_scope(owner) {
            return None;
        }
        if let Some(candidate) = candidate
            && !root.is_descendant(candidate, owner)
            && follows_in_tree(root, owner, candidate) != forward
        {
            return None;
        }
        let node = root.widget_arena.nodes.find(owner)?;
        let list = node.item.widget.virtual_children()?;
        if let crate::app::InputGeometrySource::Presented(view) =
            &root.global_state.input_geometry_source
            && view.as_ref()?.get(owner)?.virtual_revision != Some(list.revision)
        {
            return None;
        }
        let stop = candidate.and_then(|candidate| {
            list.children
                .iter()
                .find(|row| root.is_descendant(candidate, row.widget))
                .map(|row| row.index)
        });
        let mut index = if forward {
            list.children.first().map_or(Some(0), |row| Some(row.index))
        } else {
            list.children
                .last()
                .map(|row| row.index)
                .or_else(|| list.total.checked_sub(1))
        };
        while let Some(next) = index.filter(|&next| next < list.total) {
            if Some(next) == stop {
                return None;
            }
            if !list.children.iter().any(|row| row.index == next) {
                return Some((owner, next));
            }
            index = if forward {
                next.checked_add(1)
            } else {
                next.checked_sub(1)
            };
        }
        None
    };
    if let crate::app::InputGeometrySource::Presented(Some(view)) =
        &root.global_state.input_geometry_source
    {
        return if forward {
            view.records()
                .iter()
                .find_map(|record| inspect(root, record.id))
        } else {
            view.records()
                .iter()
                .rev()
                .find_map(|record| inspect(root, record.id))
        };
    }
    let mut walk = std::mem::take(&mut root.global_state.navigation_walk);
    walk.clear();
    walk.push(root.global_state.modal_scope.unwrap_or(root.root_id()));
    let mut found = None;
    while let Some(owner) = walk.pop() {
        if let Some(gap) = inspect(root, owner) {
            found = Some(gap);
            break;
        }
        let children = root.widget_arena.get_node(owner).item.widget.children_ids();
        if forward {
            walk.extend(children.into_iter().rev());
        } else {
            walk.extend(children);
        }
    }
    root.global_state.navigation_walk = walk;
    found
}

/// Check logical gaps before the ordinary spatial or Tab candidate is committed.
pub(crate) fn defer_virtual(
    root: &mut RenderRoot,
    candidate: Option<WidgetId>,
    forward: bool,
    direction: Option<NavigationAction>,
) -> bool {
    let forward = if root.global_state.focused_widget.is_none() && direction.is_some() {
        true
    } else {
        forward
    };
    if let Some(pending) = root.global_state.deferred_navigation {
        if pending.forward == forward && pending.direction == direction {
            return true;
        }
        cancel_virtual(root);
    }
    let Some(anchor) = root.global_state.focused_widget else {
        if let Some((owner, index)) = entry_gap(root, candidate, forward) {
            request_virtual(
                root,
                DeferredNavigation {
                    serial: 0,
                    revision: 0,
                    owner,
                    anchor: None,
                    index,
                    forward,
                    direction,
                    selection: None,
                },
            );
            return true;
        }
        return false;
    };
    let mut row = anchor;
    while let Some(owner) = root.widget_arena.parent_of(row) {
        let widget = &root.widget_arena.get_node(owner).item.widget;
        if let Some(list) = widget.virtual_children()
            && let Some(current) = list.children.iter().find(|entry| entry.widget == row)
        {
            if let crate::app::InputGeometrySource::Presented(view) =
                &root.global_state.input_geometry_source
                && view
                    .as_ref()
                    .and_then(|view| view.get(owner))
                    .and_then(|record| record.virtual_revision)
                    != Some(list.revision)
            {
                return true;
            }
            // Preserve normal navigation among controls inside a logical row.
            if candidate.is_some_and(|next| {
                next != anchor
                    && root.is_descendant(next, row)
                    && (direction.is_some() || follows_in_tree(root, anchor, next) == forward)
            }) {
                return false;
            }
            let stop = candidate.and_then(|next| {
                list.children
                    .iter()
                    .find(|entry| root.is_descendant(next, entry.widget))
                    .map(|entry| entry.index)
            });
            let mut index = if forward {
                current.index.checked_add(1)
            } else {
                current.index.checked_sub(1)
            };
            while let Some(next) = index.filter(|&next| next < list.total) {
                if Some(next) == stop {
                    return false;
                }
                if !list.children.iter().any(|entry| entry.index == next) {
                    request_virtual(
                        root,
                        DeferredNavigation {
                            serial: 0,
                            revision: 0,
                            owner,
                            anchor: Some(anchor),
                            index: next,
                            forward,
                            direction,
                            selection: None,
                        },
                    );
                    return true;
                }
                index = if forward {
                    next.checked_add(1)
                } else {
                    next.checked_sub(1)
                };
            }
            return false;
        }
        row = owner;
    }
    false
}

/// Only accepted presentation can complete a request for a newly mounted row.
pub(crate) fn resume_virtual(root: &mut RenderRoot) {
    let Some(mut request) = root.global_state.deferred_navigation else {
        return;
    };
    if root.global_state.focused_widget != request.anchor || !root.focus_in_scope(request.owner) {
        cancel_virtual(root);
        return;
    }
    if let Some(group) = request.selection
        && request
            .anchor
            .and_then(|id| super::selection::member(root, id))
            .map(|member| member.id)
            != Some(group)
    {
        cancel_virtual(root);
        return;
    }
    let source = root.global_state.input_geometry_source.clone();
    let view = source.borrow();
    let Some(node) = root.widget_arena.nodes.find(request.owner) else {
        return;
    };
    let Some(list) = node.item.widget.virtual_children() else {
        return;
    };

    if let GeometryView::Presented(view) = view
        && view
            .and_then(|view| view.get(request.owner))
            .and_then(|record| record.virtual_revision)
            != Some(list.revision)
    {
        return;
    }
    let Some((serial, resolved, forward)) = list.target else {
        if list.revision != request.revision {
            cancel_virtual(root);
        }
        return;
    };
    if serial != request.serial || forward != request.forward {
        return;
    }
    if !list.target_ready {
        return;
    }
    request.index = resolved;
    let total = list.total;
    let Some(row) = list
        .children
        .iter()
        .find(|row| row.index == request.index)
        .map(|row| row.widget)
    else {
        return;
    };
    if !root.input_in_scope(row, view) {
        return;
    }
    let next = match request.selection {
        Some(group) => super::selection::first_in_row(root, row, group, request.forward),
        None => find_first_focusable(root, &[], row, request.forward),
    };
    if let Some(next) = next {
        cancel_virtual(root);
        cancel(root);
        root.global_state.next_focused_widget = Some(next);
        if let Some(group) = request.selection {
            super::selection::select(root, next, group);
        }
        return;
    }
    let next = if request.forward {
        request.index.checked_add(1)
    } else {
        request.index.checked_sub(1)
    };
    if let Some(next) = next.filter(|&next| next < total) {
        request.index = next;
        request_virtual(root, request);
    } else {
        cancel_virtual(root);
        if let Some(group) = request.selection {
            super::selection::finish_virtual(
                root,
                request.owner,
                group,
                request.forward,
                request.direction.expect("group navigation has a direction"),
            );
            return;
        }
        root.global_state.next_focused_widget = match request.direction {
            Some(direction) => directional_focus(root, direction, view),
            None => super::update::find_next_focusable(root, request.forward),
        };
    }
}

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
    cancel_virtual(root);
    root.global_state.navigation_directions.clear();
    cancel(root);
}
pub(crate) fn cancel_gated(root: &mut RenderRoot) {
    let source = root.global_state.input_geometry_source.clone();
    let view = source.borrow();
    if root
        .global_state
        .deferred_navigation
        .is_some_and(|request| root.input_is_gated(request.owner, view))
    {
        cancel_virtual(root);
    }
    if root
        .global_state
        .navigation_target
        .is_some_and(|id| root.input_is_gated(id, view))
    {
        cancel(root);
    }
    if root
        .global_state
        .focused_widget
        .or(root.global_state.focus_fallback)
        .is_some_and(|id| root.input_is_gated(id, view))
    {
        root.global_state.navigation_directions.clear();
    }
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
    if matches!(event.phase, ActionPhase::Pressed | ActionPhase::Repeated) {
        super::event::set_focus_visible(root, true);
    }
    let source = root.global_state.input_geometry_source.clone();
    let view = source.borrow();
    if matches!(view, GeometryView::Presented(None)) {
        return Handled::Yes;
    }
    if event.action == NavigationAction::Confirm && root.global_state.deferred_navigation.is_some()
    {
        cancel(root);
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
            root.global_state.next_focused_widget = find_first_focusable(root, &[], scope, true)
                .map(|id| super::selection::entry(root, id));
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
        if super::selection::navigate(root, event.action) {
            return Handled::Yes;
        }
        let next = directional_focus(root, event.action, view);
        let deferred = matches!(event.action, NavigationAction::Up | NavigationAction::Down)
            && defer_virtual(
                root,
                next,
                event.action == NavigationAction::Down,
                Some(event.action),
            );
        if !deferred && next.is_some() {
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
        return find_first_focusable(root, &[], scope, true)
            .map(|id| super::selection::entry(root, id));
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
