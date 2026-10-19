// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Cursor selection borrows the same accepted mouse path as ordinary input.
use super::{GeometryView, RenderRoot, RenderRootSignal};
use crate::core::{PointerType, WidgetId};
use kurbo::Point;

#[derive(Default)]
pub(crate) struct CursorState {
    pub(crate) mouse_active: bool,
    pub(crate) overlay: Option<(WidgetId, Point)>,
}

impl RenderRoot {
    fn cursor_child(&self, owner: WidgetId, view: GeometryView<'_>) -> Option<WidgetId> {
        if !self.is_still_interactive(owner) || !self.input_in_scope(owner, view) {
            return None;
        }
        let owner_node = self.widget_arena.nodes.find(owner)?;
        let available = |id| {
            let Some(node) = self.widget_arena.nodes.find(id) else {
                return false;
            };
            node.item.widget.is_cursor()
                && !node.item.state.is_stashed
                && !node.item.state.is_disabled
                && self.widget_arena.parent_of(id) == Some(owner)
        };
        match view {
            GeometryView::Presented(Some(view)) => {
                let record = view.get(owner)?;
                if record.input_revision != owner_node.item.state.input_revision {
                    return None;
                }
                view.children_of(owner).rev().find(|&id| {
                    available(id)
                        && view
                            .get(id)
                            .is_some_and(|record| record.is_cursor && !record.is_stashed)
                })
            }
            GeometryView::Presented(None) => None,
            GeometryView::Live => owner_node
                .item
                .widget
                .children_ids()
                .into_iter()
                .rev()
                .find(|&id| available(id)),
        }
    }

    pub(crate) fn refresh_cursor(&mut self, view: GeometryView<'_>) {
        let next = (|| {
            if !self.global_state.cursor.mouse_active || !self.global_state.window_focused {
                return None;
            }
            let mouse = std::iter::once(&self.global_state.pointer)
                .chain(&self.global_state.other_pointers)
                .find(|pointer| pointer.info.pointer_type == PointerType::Mouse)?;
            let physical = mouse.last_pointer_physical?;
            let logical = physical.to_logical(view.scale(self.global_state.scale_factor));
            let path = if mouse.pointer_capture_target.is_some() {
                &mouse.active_path
            } else {
                &mouse.hovered_path
            };
            let cursor = path
                .iter()
                .find_map(|&owner| self.cursor_child(owner, view))?;
            Some((cursor, Point::new(logical.x, logical.y)))
        })();
        let previous = self.global_state.cursor.overlay;
        if previous == next {
            return;
        }
        self.global_state.cursor.overlay = next;
        for (id, _) in previous.into_iter().chain(next) {
            if self.has_widget(id) {
                let state = self.widget_arena.get_state_mut(id);
                state.transform_changed = true;
                state.needs_compose = true;
                crate::passes::merge_state_up(&mut self.widget_arena, id);
            }
        }
        if previous.is_some() != next.is_some() {
            self.global_state
                .emit_signal(RenderRootSignal::SetCursorVisible(next.is_none()));
        }
        self.root_state_mut().needs_paint = true;
        self.global_state
            .emit_signal(RenderRootSignal::RequestRedraw);
    }
}
