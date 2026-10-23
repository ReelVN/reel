//! Describe controls from the committed author tree without retaining its nodes.
use super::{NodeId, NodeKind, SceneWidget, StageView};
use reel_ui::{
    app::PresentedGeometryView,
    core::WidgetId,
    input::{Binding, Control},
};

pub(crate) struct ControlCandidate {
    pub node: NodeId,
    pub key: String,
    pub state: String,
    pub fraction: [f64; 2],
    pub accepts_text: bool,
    pub submits_text: bool,
    pub binding: Option<Binding>,
    pub binding_value: f32,
}

impl StageView {
    /// Find the same first mapped input scope reached by ordinary event bubbling.
    pub(crate) fn binding_reaches(
        root: &reel_ui::app::RenderRoot,
        geometry: &PresentedGeometryView,
        from: WidgetId,
        target: WidgetId,
        control: Control,
    ) -> bool {
        let mut record = geometry.get(from);
        while let Some(current) = record {
            if let Some(widget) = root
                .get_widget(current.id)
                .and_then(|widget| widget.downcast::<SceneWidget>())
            {
                if matches!(control, Control::Key(_)) && widget.editor.is_some() {
                    return false;
                }
                if widget.is_button()
                    && matches!(
                        control,
                        Control::Key(
                            reel_ui::core::keyboard::Code::Enter
                                | reel_ui::core::keyboard::Code::NumpadEnter
                                | reel_ui::core::keyboard::Code::Space
                        ) | Control::Button(reel_ui::input::GamepadButton::South)
                    )
                {
                    return false;
                }
                if widget.slider.is_some()
                    && matches!(
                        control,
                        Control::Key(
                            reel_ui::core::keyboard::Code::ArrowLeft
                                | reel_ui::core::keyboard::Code::ArrowRight
                                | reel_ui::core::keyboard::Code::ArrowUp
                                | reel_ui::core::keyboard::Code::ArrowDown
                                | reel_ui::core::keyboard::Code::Home
                                | reel_ui::core::keyboard::Code::End
                        )
                    )
                {
                    return false;
                }
                if let Some(input) = &widget.input {
                    if input.binds(control) {
                        return current.id == target;
                    }
                } else if matches!(
                    control,
                    Control::Pointer(reel_ui::core::PointerButton::Primary)
                ) && (widget.is_button()
                    || widget.editor.is_some()
                    || widget.slider.is_some())
                {
                    return false;
                }
            }
            record = current.parent.map(|index| &geometry.records()[index]);
        }
        false
    }
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
            let state = match &node.node.kind {
                NodeKind::Slider { value, .. } => value.to_string(),
                NodeKind::Toggle { value, .. } => value.to_string(),
                NodeKind::Radio { selected, .. } => selected.to_string(),
                NodeKind::TextInput { value, .. } => value.to_string(),
                _ => String::new(),
            };
            let mut emit = |kind: &str,
                            detail: &str,
                            fraction,
                            accepts_text,
                            submits_text,
                            binding: Option<(Binding, f32)>| {
                let (binding, binding_value) =
                    binding.map_or((None, 1.), |(binding, value)| (Some(binding), value));
                output.push(ControlCandidate {
                    node: node.node.id,
                    key: format!("{path}:{kind}:{detail}"),
                    state: state.clone(),
                    fraction,
                    accepts_text,
                    submits_text,
                    binding,
                    binding_value,
                });
            };
            match &node.node.kind {
                NodeKind::Button {
                    label,
                    listening: true,
                    ..
                } => {
                    emit(
                        "button",
                        label.as_deref().unwrap_or(""),
                        [0.5, 0.5],
                        false,
                        false,
                        None,
                    );
                }
                NodeKind::Toggle {
                    label,
                    listening: true,
                    ..
                } => {
                    emit(
                        "toggle",
                        label.as_deref().unwrap_or(""),
                        [0.5, 0.5],
                        false,
                        false,
                        None,
                    );
                }
                NodeKind::Radio { label, .. } => {
                    emit(
                        "radio",
                        label.as_deref().unwrap_or(""),
                        [0.5, 0.5],
                        false,
                        false,
                        None,
                    );
                }
                NodeKind::Slider {
                    listening: true, ..
                } => {
                    for (name, x) in [("low", 0.05), ("middle", 0.5), ("high", 0.95)] {
                        emit("slider", name, [x, 0.5], false, false, None);
                    }
                }
                NodeKind::TextInput { value, options, .. }
                    if value.is_empty() && !options.read_only =>
                {
                    emit("text", "empty", [0.5, 0.5], true, false, None);
                }
                NodeKind::TextInput {
                    value,
                    multiline: false,
                    options,
                } if !value.is_empty() && options.submit_listening => {
                    emit("text", "submit", [0.5, 0.5], false, true, None);
                }
                NodeKind::Input {
                    bindings,
                    listening: true,
                    ..
                } => {
                    for action in bindings.iter() {
                        for (index, binding) in action.bindings.iter().enumerate() {
                            let values: &[f32] = if matches!(binding.control, Control::Axis(_)) {
                                &[1., -1.]
                            } else {
                                &[1.]
                            };
                            for &value in values {
                                if binding.scale == 0.
                                    || (binding.positive_only && value * binding.scale <= 0.)
                                {
                                    continue;
                                }
                                let detail = format!(
                                    "{}:{index}:{:?}:{}:{}:{}:{value}",
                                    action.name,
                                    binding.control,
                                    binding.scale,
                                    binding.dead_zone,
                                    binding.positive_only
                                );
                                emit(
                                    "input",
                                    &detail,
                                    [0.5, 0.5],
                                    false,
                                    false,
                                    Some((*binding, value)),
                                );
                            }
                        }
                    }
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
