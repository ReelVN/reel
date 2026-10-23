//! Describe controls from the committed author tree without retaining its nodes.
use super::{NodeId, NodeKind, StageView};

pub(crate) struct ControlCandidate {
    pub node: NodeId,
    pub key: String,
    pub fraction: [f64; 2],
    pub accepts_text: bool,
}

impl StageView {
    pub(crate) fn control_candidates(&self) -> Vec<ControlCandidate> {
        let mut output = Vec::new();
        let Some(root) = self.root.as_deref() else {
            return output;
        };
        let mut pending = vec![(root, String::from("0"))];
        while let Some((node, path)) = pending.pop() {
            if !node.node.props.visible || !node.node.props.enabled {
                continue;
            }
            let mut emit = |kind: &str, detail: &str, fraction, accepts_text| {
                output.push(ControlCandidate {
                    node: node.node.id,
                    key: format!("{path}:{kind}:{detail}"),
                    fraction,
                    accepts_text,
                });
            };
            match &node.node.kind {
                NodeKind::Button {
                    label,
                    listening: true,
                    ..
                } => {
                    emit("button", label.as_deref().unwrap_or(""), [0.5, 0.5], false);
                }
                NodeKind::Toggle {
                    label,
                    value,
                    listening: true,
                    ..
                } => {
                    emit(
                        "toggle",
                        &format!("{}:{value}", label.as_deref().unwrap_or("")),
                        [0.5, 0.5],
                        false,
                    );
                }
                NodeKind::Radio {
                    label, selected, ..
                } => {
                    emit(
                        "radio",
                        &format!("{}:{selected}", label.as_deref().unwrap_or("")),
                        [0.5, 0.5],
                        false,
                    );
                }
                NodeKind::Slider {
                    value,
                    listening: true,
                    ..
                } => {
                    for (name, x) in [("low", 0.05), ("middle", 0.5), ("high", 0.95)] {
                        emit("slider", &format!("{name}:{value}"), [x, 0.5], false);
                    }
                }
                NodeKind::TextInput { value, options, .. }
                    if value.is_empty() && !options.read_only =>
                {
                    emit("text", "empty", [0.5, 0.5], true);
                }
                _ => {}
            }
            for (index, child) in node.children.iter().enumerate().rev() {
                pending.push((child.as_ref(), format!("{path}/{index}")));
            }
        }
        output
    }
}
