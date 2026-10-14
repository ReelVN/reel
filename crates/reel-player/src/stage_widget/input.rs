use reel_ui::{
    core::{PointerButton, PointerEvent, pointer_info},
    input::{ActionMap, Control, InputDeviceId, InputEvent},
    stage::{InputBindings, NodeAction, NodeEvent, NodeId},
};

pub(super) struct NodeInput {
    pub bindings: InputBindings,
    map: ActionMap<usize>,
}
impl NodeInput {
    pub fn new(bindings: InputBindings) -> Self {
        let map = ActionMap::new(
            bindings
                .iter()
                .enumerate()
                .map(|(i, a)| (i, a.bindings.clone())),
        )
        .expect("validated stage bindings");
        Self { bindings, map }
    }
    pub fn binds(&self, control: Control) -> bool {
        self.map.binds(control)
    }
    pub fn update(
        &mut self,
        node: NodeId,
        event: &InputEvent,
        mut emit: impl FnMut(NodeEvent),
    ) -> bool {
        if let InputEvent::Value { control, .. } = event
            && !self.map.binds(*control)
        {
            return false;
        }
        let bindings = &self.bindings;
        let result = self.map.update(event, |e| {
            emit(NodeEvent::Action(NodeAction {
                node,
                action: bindings[e.action].name.clone(),
                value: e.value,
                phase: e.phase,
                bindings: bindings.clone(),
            }))
        });
        result.is_ok() && matches!(event, InputEvent::Value { .. })
    }
    pub fn cancel(&mut self, node: NodeId, emit: impl FnMut(NodeEvent)) {
        self.update(node, &InputEvent::Cancel, emit);
    }
    pub fn pointer(
        &mut self,
        node: NodeId,
        event: &PointerEvent,
        emit: impl FnMut(NodeEvent),
    ) -> bool {
        let device = InputDeviceId::Pointer(pointer_info(event));
        let input = match event {
            PointerEvent::Down(e) | PointerEvent::Up(e) => InputEvent::Value {
                device,
                control: Control::Pointer(e.button.unwrap_or(PointerButton::Primary)),
                value: if matches!(event, PointerEvent::Down(_)) {
                    1.
                } else {
                    0.
                },
            },
            PointerEvent::Cancel(_) => InputEvent::Disconnected(device),
            _ => return false,
        };
        self.update(node, &input, emit)
    }
}
