//! Host inspection uses the same accepted geometry and input gates as pointer events.
use super::{PresentationBridge, SessionUi, WidgetId};
use reel_ui::{app::PresentedGeometryView, kurbo::Point};

/// An actionable control in the accepted frame. Coordinates are window pixels.
/// `key` identifies its authored tree path and control content, independently of
/// transient widget allocations. `accepts_text` marks an empty editable field.
#[derive(Clone, Debug)]
pub struct InputTarget {
    pub key: String,
    pub position: [f64; 2],
    pub accepts_text: bool,
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
    /// Enumerate visible, enabled controls with an unobstructed accepted hit point.
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
            let point = if reaches(geometry, preferred, widget) {
                Some(preferred)
            } else if candidate.fraction == [0.5, 0.5] {
                [[0.25, 0.25], [0.75, 0.25], [0.25, 0.75], [0.75, 0.75]]
                    .into_iter()
                    .map(at)
                    .find(|&point| reaches(geometry, point, widget))
            } else {
                None
            };
            if let Some(point) = point {
                targets.push(InputTarget {
                    key: candidate.key,
                    position: [point.x, point.y],
                    accepts_text: candidate.accepts_text,
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
