use reel_ui::{
    core::{
        PointerButton, PointerEvent,
        keyboard::{KeyState, KeyboardEvent},
        pointer_info,
    },
    input::{ActionMap, ActionPhase, Control, InputDeviceId, InputEvent},
    stage::{InputBindings, NodeAction, NodeEvent, NodeId, NodeKey},
};

pub(super) struct NodeInput {
    pub bindings: InputBindings,
    map: ActionMap<usize>,
    pub keyboard: bool,
    held_keys: Vec<KeyboardEvent>,
}
impl NodeInput {
    pub fn new(bindings: InputBindings, keyboard: bool) -> Self {
        let map = ActionMap::new(
            bindings
                .iter()
                .enumerate()
                .map(|(i, a)| (i, a.bindings.clone())),
        )
        .expect("validated stage bindings");
        Self {
            bindings,
            map,
            keyboard,
            held_keys: Vec::new(),
        }
    }
    pub fn key(
        &mut self,
        node: NodeId,
        event: &KeyboardEvent,
        mut emit: impl FnMut(NodeEvent),
    ) -> bool {
        if !self.keyboard || event.is_composing {
            return false;
        }
        let held = self.held_keys.iter().position(|key| key.code == event.code);
        let phase = match (event.state, held) {
            (KeyState::Down, None) if !event.repeat => {
                self.held_keys.push(event.clone());
                ActionPhase::Pressed
            }
            (KeyState::Down, Some(_)) => ActionPhase::Repeated,
            (KeyState::Up, Some(index)) => {
                self.held_keys.swap_remove(index);
                ActionPhase::Released
            }
            _ => return true,
        };
        emit(NodeEvent::Key(NodeKey {
            node,
            event: event.clone(),
            phase,
        }));
        true
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
        if matches!(
            event,
            InputEvent::Cancel | InputEvent::Disconnected(InputDeviceId::Keyboard)
        ) {
            self.cancel_keys(node, &mut emit);
        }
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
    pub fn cancel_keys(&mut self, node: NodeId, mut emit: impl FnMut(NodeEvent)) {
        for mut event in self.held_keys.drain(..) {
            event.state = KeyState::Up;
            event.repeat = false;
            emit(NodeEvent::Key(NodeKey {
                node,
                event,
                phase: ActionPhase::Cancelled,
            }));
        }
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
