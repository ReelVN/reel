// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Input scopes reuse the accepted geometry and ordinary focus/capture passes.
use super::{GeometryView, InputGeometrySource, RenderRoot};
use crate::core::WidgetId;

impl RenderRoot {
    pub(crate) fn input_scope(&self, view: GeometryView<'_>) -> Option<WidgetId> {
        match view {
            GeometryView::Live => self.global_state.modal_scope,
            GeometryView::Presented(Some(view)) => view.modal_scope(),
            GeometryView::Presented(None) => None,
        }
    }

    pub(crate) fn is_descendant(&self, id: WidgetId, ancestor: WidgetId) -> bool {
        let mut current = Some(id);
        while let Some(id) = current {
            if id == ancestor {
                return true;
            }
            current = self.widget_arena.parent_of(id);
        }
        false
    }

    pub(crate) fn input_in_scope(&self, id: WidgetId, view: GeometryView<'_>) -> bool {
        match view {
            GeometryView::Presented(None) => false,
            GeometryView::Presented(Some(view)) => view.get(id).is_some_and(|record| {
                !record.is_stashed
                    && view
                        .modal_scope()
                        .is_none_or(|scope| view.contains_in_scope(id, scope))
            }),
            GeometryView::Live => self
                .global_state
                .modal_scope
                .is_none_or(|scope| self.is_descendant(id, scope)),
        }
    }

    pub(crate) fn focus_in_scope(&self, id: WidgetId) -> bool {
        self.is_still_interactive(id)
            && match &self.global_state.input_geometry_source {
                // Losing presentation blocks delivery, not the remembered focus.
                InputGeometrySource::Presented(None) => true,
                source => self.input_in_scope(id, source.borrow()),
            }
    }

    pub(crate) fn refresh_live_input_scope(&mut self) -> bool {
        if !std::mem::take(&mut self.global_state.modal_dirty)
            || !matches!(
                self.global_state.input_geometry_source,
                InputGeometrySource::Live
            )
        {
            return false;
        }
        let mut modal = None;
        let mut pending = vec![self.root_id()];
        while let Some(id) = pending.pop() {
            let node = self.widget_arena.get_node(id);
            if node.item.state.is_stashed || node.item.state.in_cursor {
                continue;
            }
            if node.item.state.is_modal {
                modal = Some(id);
            }
            pending.extend(node.item.widget.children_ids().into_iter().rev());
        }
        self.set_input_scope(modal)
    }

    pub(crate) fn accept_input_scope(&mut self) {
        match &self.global_state.input_geometry_source {
            InputGeometrySource::Presented(Some(view)) => {
                self.set_input_scope(view.modal_scope());
            }
            InputGeometrySource::Presented(None) => {}
            InputGeometrySource::Live => {
                self.global_state.modal_dirty = true;
                self.refresh_live_input_scope();
            }
        }
    }

    fn set_input_scope(&mut self, next: Option<WidgetId>) -> bool {
        let previous = self.global_state.modal_scope;
        let changed = next != previous;
        if changed {
            crate::passes::navigation::cancel_all(self);
            crate::passes::event::run_on_text_event_pass(
                self,
                &crate::core::TextEvent::Input(crate::input::InputEvent::Cancel),
            );
            let focus = self.global_state.focused_widget;
            if let Some(previous) = previous {
                if self.has_widget(previous)
                    && focus
                        .is_some_and(|id| self.has_widget(id) && self.is_descendant(id, previous))
                {
                    self.widget_arena.get_state_mut(previous).modal_focus = focus;
                }
            } else if next.is_some() {
                self.global_state.modal_base_focus = focus;
            }
            self.global_state.modal_scope = next;
            self.global_state.needs_pointer_pass = true;
        }
        if let Some(scope) = next {
            if !changed
                && self
                    .global_state
                    .next_focused_widget
                    .is_some_and(|id| self.focus_in_scope(id))
            {
                return false;
            }
            let focus = if self.has_widget(scope) {
                self.widget_arena
                    .get_state(scope)
                    .modal_focus
                    .filter(|&id| self.focus_in_scope(id))
                    .or_else(|| crate::passes::update::find_first_focusable(self, &[], scope, true))
            } else {
                None
            };
            self.global_state.next_focused_widget = focus;
            self.global_state.focus_anchor = focus;
            true
        } else if changed {
            let focus = self
                .global_state
                .modal_base_focus
                .take()
                .filter(|&id| self.focus_in_scope(id));
            self.global_state.next_focused_widget = focus;
            self.global_state.focus_anchor = focus;
            true
        } else {
            false
        }
    }
}
