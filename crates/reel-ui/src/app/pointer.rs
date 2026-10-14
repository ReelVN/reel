use super::{RenderRoot, RenderRootState};
use crate::core::{PointerEvent, PointerId, PointerInfo, PointerType, WidgetId};
use crate::dpi::{LogicalPosition, PhysicalPosition};

pub(crate) struct PointerContact {
    pub(crate) info: PointerInfo,
    pub(crate) last_mouse_pos: Option<LogicalPosition<f64>>,
    pub(crate) last_pointer_physical: Option<PhysicalPosition<f64>>,
    pub(crate) hovered_path: Vec<WidgetId>,
    pub(crate) active_path: Vec<WidgetId>,
    pub(crate) pointer_capture_target: Option<WidgetId>,
    pub(crate) pan_target: Option<WidgetId>,
    pub(crate) pointer_capture_revision: u64,
    pub(crate) pointer_capture_allows_pan: bool,
    pub(crate) pointer_capture_terminal_delivered: Option<WidgetId>,
}

impl PointerContact {
    pub(crate) fn new(info: PointerInfo, depth: usize) -> Self {
        Self {
            info,
            last_mouse_pos: None,
            last_pointer_physical: None,
            hovered_path: Vec::with_capacity(depth),
            active_path: Vec::with_capacity(depth),
            pointer_capture_target: None,
            pan_target: None,
            pointer_capture_revision: 0,
            pointer_capture_allows_pan: false,
            pointer_capture_terminal_delivered: None,
        }
    }

    pub(crate) fn mouse(depth: usize) -> Self {
        Self::new(
            PointerInfo {
                pointer_id: Some(PointerId::PRIMARY),
                persistent_device_id: None,
                pointer_type: PointerType::Mouse,
            },
            depth,
        )
    }

    pub(crate) fn path(&self, active: bool) -> &[WidgetId] {
        if active {
            &self.active_path
        } else {
            &self.hovered_path
        }
    }
}

impl RenderRootState {
    pub(crate) fn swap_pointer(&mut self, index: usize) {
        std::mem::swap(&mut self.pointer, &mut self.other_pointers[index]);
    }

    pub(crate) fn other_pointer_has(&self, id: WidgetId, active: bool, descendants: bool) -> bool {
        self.other_pointers.iter().any(|p| {
            let path = p.path(active);
            if descendants {
                path.contains(&id)
            } else {
                path.first() == Some(&id)
            }
        })
    }
}

impl RenderRoot {
    pub(crate) fn select_pointer(&mut self, info: PointerInfo, start: bool) -> bool {
        let state = &mut self.global_state;
        if state.pointer.info == info {
            return true;
        }
        if let Some(i) = state.other_pointers.iter().position(|p| p.info == info) {
            state.swap_pointer(i);
        } else {
            // A contact is born on Down. An orphaned touch Move/Up cannot
            // acquire ownership after a suspension or manufacture a click.
            if info.pointer_type == PointerType::Touch && !start {
                return false;
            }
            let contact = PointerContact::new(info, state.max_input_path);
            state.other_pointers.reserve_exact(1);
            state
                .other_pointers
                .push(std::mem::replace(&mut state.pointer, contact));
        }
        true
    }

    pub(crate) fn retire_touch(&mut self) {
        if self.global_state.pointer.info.pointer_type != PointerType::Touch {
            return;
        }
        // The permanent mouse slot means removing a touch always has a successor.
        let contact = self
            .global_state
            .other_pointers
            .pop()
            .expect("mouse pointer slot");
        self.global_state.pointer = contact;
    }

    /// Cancel every pointer using its original identity. Hosts use this on
    /// suspension or device reset; ordinary Cancel events address one pointer.
    pub fn cancel_pointer_inputs(&mut self) {
        let source = self.global_state.input_geometry_source.clone();
        let count = self.global_state.other_pointers.len();
        for i in 0..=count {
            if i < count {
                self.global_state.swap_pointer(i);
            }
            let event = PointerEvent::Cancel(self.global_state.pointer.info);
            crate::passes::event::run_on_pointer_event_with_view(self, &event, source.borrow());
            if i < count {
                self.global_state.swap_pointer(i);
            }
        }
        crate::passes::update::transition_pointer_geometry(self, source.borrow(), source.borrow());
        let primary = PointerInfo {
            pointer_id: Some(PointerId::PRIMARY),
            persistent_device_id: None,
            pointer_type: PointerType::Mouse,
        };
        if self.global_state.pointer.info != primary {
            let index = self
                .global_state
                .other_pointers
                .iter()
                .position(|p| p.info == primary)
                .expect("mouse pointer slot");
            self.global_state.swap_pointer(index);
        }
        self.global_state.other_pointers.clear();
        self.run_rewrite_passes();
    }

    /// Capture for a particular device/contact, independent of the last event.
    pub fn pointer_capture_for(&self, pointer: PointerInfo) -> Option<WidgetId> {
        std::iter::once(&self.global_state.pointer)
            .chain(&self.global_state.other_pointers)
            .find(|p| p.info == pointer)
            .and_then(|p| p.pointer_capture_target)
    }
}
