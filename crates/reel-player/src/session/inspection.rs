//! Host inspection uses the same accepted geometry and input gates as pointer events.
use super::{PresentationBridge, SessionUi, WidgetId};
use reel_ui::{
    app::PresentedGeometryView,
    input::{Binding, Control},
    kurbo::Point,
};

/// Host focus and primary mouse position retained across development checkpoints.
/// Locations use authored child paths and widget roles, never allocated widget IDs.
/// Ordinary save loading continues to start with cleared host input ownership.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputFocus {
    focused: Option<crate::stage_widget::FocusPath>,
    anchor: Option<crate::stage_widget::FocusPath>,
    visible: bool,
    mouse: Option<[u64; 2]>,
}

/// An actionable control in the accepted frame. Coordinates are window pixels.
/// `key` identifies its authored tree path and control content, independently of
/// transient widget allocations. `accepts_text` marks an empty editable field;
/// `submits_text` marks a nonempty single-line field with a submit callback.
#[derive(Clone, Debug)]
pub struct InputTarget {
    pub key: String,
    /// Current editable value for strict playback, separate from target identity.
    pub state: String,
    pub position: [f64; 2],
    pub accepts_text: bool,
    /// The focused field can submit its current value with an ordinary Enter key.
    pub submits_text: bool,
    /// Original action mapping on the input scope that owns this accepted route.
    pub binding: Option<Binding>,
    /// Normalized device value to press before releasing that mapped control.
    pub binding_value: f32,
}

fn reaches(view: &PresentedGeometryView, point: Point, target: WidgetId) -> bool {
    let Some(hit) = view.hit_test(point) else {
        return false;
    };
    let mut record = view.get(hit);
    while let Some(current) = record {
        if current.id == target {
            return true;
        }
        record = current.parent.map(|index| &view.records()[index]);
    }
    false
}

impl SessionUi {
    /// Captures accepted host focus without dispatching input or author callbacks.
    /// Returns None when a remembered target is outside the authored stage.
    pub fn input_focus(&self, bridge: &PresentationBridge, root: WidgetId) -> Option<InputFocus> {
        let path = |widget| {
            let stage = self.stage.as_ref()?;
            let geometry = bridge.accepted_geometry(bridge.accepted_key()?)?;
            stage.capture_focus_path(bridge.root(), geometry, widget, |node| {
                Self::projected_node(bridge, root, node)
            })
        };
        let focused = match bridge.root().focused_widget() {
            Some(widget) => Some(path(widget)?),
            None => None,
        };
        let anchor = match bridge.root().focus_anchor() {
            Some(widget) => Some(path(widget)?),
            None => None,
        };
        Some(InputFocus {
            focused,
            anchor,
            visible: bridge.root().focus_visible(),
            mouse: bridge
                .root()
                .mouse_position()
                .map(|point| [point.x.to_bits(), point.y.to_bits()]),
        })
    }

    /// Restores a development checkpoint's host focus before its first presentation.
    /// The VM checkpoint must retain its interaction feedback. Rebuilding widgets
    /// emits transport feedback, which is discarded here instead of replaying it
    /// as fresh author interaction callbacks. Other queued events are preserved.
    pub fn restore_input_focus(
        &self,
        bridge: &mut PresentationBridge,
        root: WidgetId,
        focus: &InputFocus,
    ) -> bool {
        let resolve = |path| {
            self.stage
                .as_ref()?
                .resolve_focus_path(bridge.root(), path, |node| {
                    Self::projected_node(bridge, root, node)
                })
        };
        let focused = match focus.focused.as_ref().map(resolve) {
            Some(Some(widget)) => Some(widget),
            Some(None) => return false,
            None => None,
        };
        let anchor = match focus.anchor.as_ref().map(resolve) {
            Some(Some(widget)) => Some(widget),
            Some(None) => return false,
            None => None,
        };
        if !bridge.restore_focus(focused, anchor, focus.visible) {
            return false;
        }
        self.events
            .borrow_mut()
            .retain(|event| !matches!(event, reel_ui::stage::NodeEvent::Interaction(_)));
        true
    }

    /// Rebuilds the checkpoint's primary hover route through the new accepted
    /// geometry, without dispatching new pointer or interaction callbacks.
    /// Existing non-interaction events from tree reconstruction remain queued.
    pub fn restore_input_pointer(
        &self,
        bridge: &mut PresentationBridge,
        focus: &InputFocus,
    ) -> Result<(), super::Error> {
        use reel_ui::{
            core::{
                PointerEvent, PointerId, PointerInfo, PointerState, PointerType, PointerUpdate,
            },
            dpi::PhysicalPosition,
        };
        let Some([x, y]) = focus.mouse else {
            return Ok(());
        };
        let scale = bridge
            .accepted_key()
            .and_then(|key| bridge.accepted_geometry(key))
            .ok_or("checkpoint pointer restore requires accepted geometry")?
            .scale_factor();
        let count = self.events.borrow().len();
        let result = bridge.handle_pointer_event(PointerEvent::Move(PointerUpdate {
            pointer: PointerInfo {
                pointer_id: Some(PointerId::PRIMARY),
                persistent_device_id: None,
                pointer_type: PointerType::Mouse,
            },
            current: PointerState {
                position: PhysicalPosition::new(
                    f64::from_bits(x) * scale,
                    f64::from_bits(y) * scale,
                ),
                ..Default::default()
            },
            coalesced: Vec::new(),
            predicted: Vec::new(),
        }));
        self.events.borrow_mut().truncate(count);
        result.map(|_| ()).map_err(|error| error.to_string().into())
    }

    /// Enumerate controls through accepted pointer geometry or the current keyboard route.
    /// Mapped keyboard and gamepad targets retain the existing focus and fallback owner.
    /// Calling this does not dispatch input or execute author callbacks.
    pub fn input_targets(&self, bridge: &PresentationBridge, root: WidgetId) -> Vec<InputTarget> {
        if self.images_pending() || self.binding.borrow().control_accepted.is_none() {
            return Vec::new();
        }
        let Some(stage) = &self.stage else {
            return Vec::new();
        };
        let Some(geometry) = bridge
            .accepted_key()
            .and_then(|key| bridge.accepted_geometry(key))
        else {
            return Vec::new();
        };
        let mut targets = Vec::new();
        for candidate in stage.control_candidates() {
            let Some(widget) = Self::projected_node(bridge, root, candidate.node) else {
                continue;
            };
            if !bridge.root().permits_input(widget) {
                continue;
            }
            let Some(record) = geometry.get(widget) else {
                continue;
            };
            if record.is_stashed || record.size.width <= 0. || record.size.height <= 0. {
                continue;
            }
            let at = |fraction: [f64; 2]| {
                record.window_transform
                    * Point::new(
                        record.size.width * fraction[0],
                        record.size.height * fraction[1],
                    )
            };
            let preferred = at(candidate.fraction);
            let binding_owner = |from, control| {
                crate::stage_widget::StageView::binding_reaches(
                    bridge.root(),
                    geometry,
                    from,
                    widget,
                    control,
                )
            };
            let pointer_reaches = |point| {
                if let Some(binding) = candidate.binding {
                    geometry
                        .hit_test(point)
                        .is_some_and(|hit| binding_owner(hit, binding.control))
                } else {
                    reaches(geometry, point, widget)
                }
            };
            let point = if let Some(binding) = candidate.binding
                && !matches!(binding.control, Control::Pointer(_))
            {
                bridge
                    .root()
                    .focused_widget()
                    .or(self.input_fallback)
                    .filter(|&route| binding_owner(route, binding.control))
                    .map(|_| preferred)
            } else if pointer_reaches(preferred) {
                Some(preferred)
            } else if candidate.fraction == [0.5, 0.5] {
                [[0.25, 0.25], [0.75, 0.25], [0.25, 0.75], [0.75, 0.75]]
                    .into_iter()
                    .map(at)
                    .find(|&point| pointer_reaches(point))
            } else {
                None
            };
            if let Some(point) = point {
                targets.push(InputTarget {
                    key: candidate.key,
                    state: candidate.state,
                    position: [point.x, point.y],
                    accepts_text: candidate.accepts_text,
                    submits_text: candidate.submits_text,
                    binding: candidate.binding,
                    binding_value: candidate.binding_value,
                });
            }
        }
        targets
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tests::{accept, mount, session};
    use reel_ui::stage::{NodeKind, NodeProps, Stage};

    #[test]
    fn discovered_targets_follow_accepted_modal_hit_geometry() {
        let mut ui = SessionUi::new(session()).unwrap();
        let mut stage = Stage::default();
        let button = NodeKind::Button {
            label: None,
            listening: true,
            focus_on_pointer: true,
        };
        let props = NodeProps {
            width: Some(100.),
            height: Some(100.),
            ..Default::default()
        };
        let under = stage.create(button.clone(), props.clone(), vec![]).unwrap();
        let mut modal_props = NodeProps {
            modal: true,
            ..props.clone()
        };
        let modal = stage
            .create(button.clone(), modal_props.clone(), vec![])
            .unwrap();
        let tree = stage
            .create(NodeKind::Stack, props, vec![under, modal])
            .unwrap();
        stage.replace(tree).unwrap();
        ui.set_stage(&stage);
        let (mut bridge, root) = mount(&ui);
        ui.sync(&mut bridge, root).unwrap();
        assert!(ui.input_targets(&bridge, root).is_empty());
        accept(&mut ui, &mut bridge, root, 1);
        let targets = ui.input_targets(&bridge, root);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].key, "0/1:button:");

        modal_props.visible = false;
        stage.update(modal, button, modal_props).unwrap();
        ui.set_stage(&stage);
        assert!(ui.input_targets(&bridge, root).is_empty());
        ui.sync(&mut bridge, root).unwrap();
        accept(&mut ui, &mut bridge, root, 2);
        let targets = ui.input_targets(&bridge, root);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].key, "0/0:button:");
    }
}
