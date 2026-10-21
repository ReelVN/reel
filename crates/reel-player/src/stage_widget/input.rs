use reel_ui::{
    core::{
        EventCtx, PointerButton, PointerEvent, PointerInfo,
        keyboard::{KeyState, KeyboardEvent},
        pointer_info,
    },
    input::{ActionMap, ActionPhase, Control, InputDeviceId, InputEvent},
    kurbo::Point,
    stage::{
        InputBindings, NodeAction, NodeEvent, NodeId, NodeKey, NodePointer, NodeWheel, PointerPhase,
    },
};

pub(super) struct NodeInput {
    pub bindings: InputBindings,
    map: ActionMap<usize>,
    pub keyboard: bool,
    pub pointer_enabled: bool,
    pub wheel_enabled: bool,
    drag: Option<(PointerInfo, Point)>,
    held_keys: Vec<KeyboardEvent>,
}
impl NodeInput {
    pub fn accepts_focus(&self) -> bool {
        self.keyboard
            || self.bindings.iter().any(|action| {
                action
                    .bindings
                    .iter()
                    .any(|binding| !matches!(binding.control, Control::Pointer(_)))
            })
    }
    pub fn pointer_focus(&self, ctx: &mut EventCtx<'_>) {
        if self.accepts_focus() {
            ctx.request_focus();
        } else if let Some(focus) = ctx.focus_target_id() {
            // Pointer-only input keeps the keyboard owner when taking a drag.
            ctx.set_focus(focus);
        }
    }
    pub fn new(
        bindings: InputBindings,
        keyboard: bool,
        pointer_enabled: bool,
        wheel_enabled: bool,
    ) -> Self {
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
            pointer_enabled,
            wheel_enabled,
            drag: None,
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
    pub fn cancel(&mut self, node: NodeId, mut emit: impl FnMut(NodeEvent)) {
        if let Some((_, position)) = self.drag.take() {
            emit(NodeEvent::Pointer(NodePointer {
                node,
                position,
                phase: PointerPhase::Cancelled,
            }));
        }
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
    pub fn wheel(&self, node: NodeId, event: &PointerEvent, ctx: &mut EventCtx<'_>) -> bool {
        let PointerEvent::Scroll(event) = event else {
            return false;
        };
        if !self.wheel_enabled {
            return false;
        }
        let delta = reel_ui::widgets::wheel_delta_pixels(
            event.delta,
            ctx.scale_factor(),
            ctx.content_box().size(),
        );
        if !delta.is_finite() {
            return false;
        }
        ctx.submit_action::<NodeEvent>(NodeEvent::Wheel(NodeWheel {
            node,
            position: ctx.local_position(event.state.position),
            delta,
        }));
        ctx.set_handled();
        true
    }
    pub fn drag(&mut self, node: NodeId, event: &PointerEvent, ctx: &mut EventCtx<'_>) -> bool {
        if !self.pointer_enabled {
            return false;
        }
        let info = pointer_info(event);
        let next = match event {
            PointerEvent::Down(e)
                if self.drag.is_none()
                    && matches!(e.button, None | Some(PointerButton::Primary)) =>
            {
                self.pointer_focus(ctx);
                ctx.capture_pointer();
                (ctx.local_position(e.state.position), PointerPhase::Pressed)
            }
            PointerEvent::Move(e) if self.drag.is_some_and(|(owner, _)| owner == info) => {
                (ctx.local_position(e.current.position), PointerPhase::Moved)
            }
            PointerEvent::Up(e)
                if self.drag.is_some_and(|(owner, _)| owner == info)
                    && matches!(e.button, None | Some(PointerButton::Primary)) =>
            {
                ctx.release_pointer();
                (ctx.local_position(e.state.position), PointerPhase::Released)
            }
            PointerEvent::Cancel(_) if self.drag.is_some_and(|(owner, _)| owner == info) => {
                (self.drag.unwrap().1, PointerPhase::Cancelled)
            }
            _ => return false,
        };
        self.drag =
            matches!(next.1, PointerPhase::Pressed | PointerPhase::Moved).then_some((info, next.0));
        ctx.submit_action::<NodeEvent>(NodeEvent::Pointer(NodePointer {
            node,
            position: next.0,
            phase: next.1,
        }));
        ctx.set_handled();
        true
    }
}
