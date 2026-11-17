// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Exclusive selection reuses ordinary focus, accepted geometry and value proposals.
use crate::app::{InputGeometrySource, RenderRoot};
use crate::core::{SelectionGroup, SelectionGroupId, WidgetId};
use crate::input::NavigationAction;

pub(super) fn member(root: &RenderRoot, id: WidgetId) -> Option<SelectionGroup> {
    let node = root.widget_arena.nodes.find(id)?;
    if !node.item.state.accepts_focus || !root.focus_in_scope(id) {
        return None;
    }
    let group = node.item.widget.selection_group()?;
    if let InputGeometrySource::Presented(view) = &root.global_state.input_geometry_source {
        let record = view.as_ref()?.get(id)?;
        if record.selection_group != Some(group.id)
            || record.input_revision != node.item.state.input_revision
        {
            return None;
        }
    }
    Some(group)
}

fn members(root: &RenderRoot, group: SelectionGroupId, mut visit: impl FnMut(WidgetId, bool)) {
    let mut inspect = |id| {
        if let Some(member) = member(root, id).filter(|member| member.id == group) {
            visit(id, member.selected);
        }
    };
    if let InputGeometrySource::Presented(Some(view)) = &root.global_state.input_geometry_source {
        for id in view.navigation_ids() {
            inspect(id);
        }
    } else {
        fn walk(root: &RenderRoot, id: WidgetId, inspect: &mut impl FnMut(WidgetId)) {
            let node = root.widget_arena.get_node(id);
            if !node.item.state.descendant_is_focusable || node.item.state.is_stashed {
                return;
            }
            inspect(id);
            for child in node.item.widget.children_ids() {
                walk(root, child, inspect);
            }
        }
        walk(root, root.root_id(), &mut inspect);
    }
}

/// A group has one Tab entry, irrespective of which member the traversal reaches.
pub(super) fn entry(root: &RenderRoot, candidate: WidgetId) -> WidgetId {
    let Some(group) = member(root, candidate) else {
        return candidate;
    };
    let mut first = None;
    let mut selected = None;
    members(root, group.id, |id, active| {
        first.get_or_insert(id);
        if active && selected.is_none() {
            selected = Some(id);
        }
    });
    selected.or(first).unwrap_or(candidate)
}

/// The group's position in the Tab order is its first available member, even
/// when the selected member appears after an interleaved group or button.
pub(super) fn anchor(root: &RenderRoot, group: SelectionGroupId) -> Option<WidgetId> {
    let mut first = None;
    members(root, group, |id, _| {
        first.get_or_insert(id);
    });
    first
}

pub(super) fn first_in_row(
    root: &RenderRoot,
    row: WidgetId,
    group: SelectionGroupId,
    forward: bool,
) -> Option<WidgetId> {
    let mut found = None;
    members(root, group, |id, _| {
        if root.is_descendant(id, row) && (found.is_none() || !forward) {
            found = Some(id);
        }
    });
    found
}

/// Return a non-wrapping successor and the eventual group boundary fallback.
fn next(
    root: &RenderRoot,
    anchor: WidgetId,
    group: SelectionGroupId,
    forward: bool,
) -> (Option<WidgetId>, Option<WidgetId>) {
    let mut first = None;
    let mut last = None;
    let mut before = None;
    let mut after = None;
    let mut seen = false;
    members(root, group, |id, _| {
        first.get_or_insert(id);
        last = Some(id);
        if id == anchor {
            seen = true;
        } else if !seen {
            before = Some(id);
        } else if after.is_none() {
            after = Some(id);
        }
    });
    if forward {
        (after, first)
    } else {
        (before, last)
    }
}

pub(super) fn select(root: &mut RenderRoot, id: WidgetId, group: SelectionGroupId) {
    let Some(member) = member(root, id).filter(|member| member.id == group) else {
        return;
    };
    super::navigation::cancel(root);
    root.global_state.next_focused_widget = Some(id);
    if !member.selected && !member.tabs.is_some_and(|tabs| tabs.manual) {
        super::update::run_single_update_pass(root, Some(id), |widget, ctx, props| {
            widget.select_group_member(ctx, props);
        });
    }
}

pub(super) fn finish_virtual(
    root: &mut RenderRoot,
    owner: WidgetId,
    group: SelectionGroupId,
    forward: bool,
    direction: NavigationAction,
) {
    let Some(anchor) = root.global_state.focused_widget else {
        return;
    };
    let (_, wrap) = next(root, anchor, group, forward);
    let mut after = None;
    members(root, group, |id, _| {
        if !root.is_descendant(id, owner)
            && super::navigation::follows_in_tree(root, owner, id) == forward
            && (after.is_none() || !forward)
        {
            after = Some(id);
        }
    });
    if let Some(candidate) = after.or(wrap)
        && !super::navigation::defer_group_gap(
            root,
            owner,
            candidate,
            group,
            direction,
            after.is_none(),
            true,
        )
        && !super::navigation::defer_group_entry(
            root,
            candidate,
            group,
            forward,
            direction,
            after.is_none(),
        )
    {
        select(root, candidate, group);
    }
}

pub(super) fn navigate(root: &mut RenderRoot, direction: NavigationAction) -> bool {
    let Some(anchor) = root.global_state.focused_widget else {
        return false;
    };
    let Some(group) = member(root, anchor) else {
        return false;
    };
    if let Some(tabs) = group.tabs
        && tabs.vertical != matches!(direction, NavigationAction::Up | NavigationAction::Down)
    {
        return false;
    }
    let forward = matches!(direction, NavigationAction::Right | NavigationAction::Down);
    let (next, wrap) = next(root, anchor, group.id, forward);
    // A mounted group boundary can still be the middle of a virtual list.
    if super::navigation::defer_virtual(root, next, forward, Some(direction)) {
        super::navigation::select_deferred_member(root, group.id);
    } else if let Some(candidate) = next.or(wrap)
        && !super::navigation::defer_group_gap(
            root,
            anchor,
            candidate,
            group.id,
            direction,
            next.is_none(),
            false,
        )
        && !super::navigation::defer_group_entry(
            root,
            candidate,
            group.id,
            forward,
            direction,
            next.is_none(),
        )
    {
        select(root, candidate, group.id);
    }
    true
}
