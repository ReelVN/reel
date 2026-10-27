//! Accepted pointer, text and accessibility dispatch.
use super::*;

impl SessionUi {
    pub fn handle_pointer(
        &mut self,
        bridge: &mut PresentationBridge,
        event: PointerEvent,
    ) -> Result<UiDispatch, Error> {
        self.virtual_images_ready = false;
        if matches!(
            event,
            PointerEvent::Down(_) | PointerEvent::Up(_) | PointerEvent::Cancel(_)
        ) {
            self.pause_dialogue();
        }
        let pointer = pointer_info(&event);
        let terminal = matches!(&event, PointerEvent::Cancel(_))
            || matches!(&event, PointerEvent::Up(e) if e.state.buttons.is_empty());
        {
            let mut binding = self.binding.borrow_mut();
            if matches!(event, PointerEvent::Down(_))
                && let Some(stamp) = binding.control_accepted
            {
                let buttons = binding.accepted_buttons;
                if let Some((_, down, button_down)) = binding
                    .contacts
                    .iter_mut()
                    .find(|(info, _, _)| *info == pointer)
                {
                    *down = stamp;
                    *button_down = buttons;
                } else {
                    binding.contacts.push((pointer, stamp, buttons));
                }
            }
            binding.down = binding
                .contacts
                .iter()
                .find(|(info, _, _)| *info == pointer)
                .map(|(_, stamp, _)| *stamp);
            binding.button_down = binding
                .contacts
                .iter()
                .find(|(info, _, _)| *info == pointer)
                .and_then(|(_, _, buttons)| *buttons);
        }
        let result = bridge.handle_pointer_event(event);
        {
            let mut binding = self.binding.borrow_mut();
            binding.down = None;
            binding.button_down = None;
            if terminal || result.is_err() {
                binding.contacts.retain(|(info, _, _)| *info != pointer);
            }
        }
        let handled = result?;
        self.finish_input(handled)
    }
    pub fn handle_text(
        &mut self,
        bridge: &mut PresentationBridge,
        event: TextEvent,
    ) -> Result<UiDispatch, Error> {
        let dispatch = self.dispatch_text(bridge, event);
        self.finish_text(dispatch)
    }
    /// Route through the current widgets and authored action mappings first.
    /// Held physical values belong to widgets; only queued actions and focus
    /// changes need to mutate the session protected by snapshot capture.
    pub(crate) fn dispatch_text(
        &self,
        bridge: &mut PresentationBridge,
        event: TextEvent,
    ) -> TextDispatch {
        let was_held = self.gesture_active();
        self.binding.borrow_mut().non_pointer = true;
        let focused = match &event {
            TextEvent::WindowFocusChange(focused) => Some(*focused),
            _ => None,
        };
        let handled = bridge.handle_text_event(event);
        self.binding.borrow_mut().non_pointer = false;
        let pause_dialogue = focused.is_some() || was_held || self.gesture_active();
        if focused == Some(false) {
            let mut binding = self.binding.borrow_mut();
            binding.down = None;
            binding.contacts.clear();
        }
        TextDispatch {
            handled,
            changes_session: focused.is_some()
                || self.inputs.borrow().is_some()
                || !self.events.borrow().is_empty(),
            focused,
            pause_dialogue,
        }
    }
    pub(crate) fn finish_text(&mut self, dispatch: TextDispatch) -> Result<UiDispatch, Error> {
        if let Some(focused) = dispatch.focused {
            self.reading_focused = focused;
            if !focused {
                self.session.set_skip_reading(false, false, false);
            }
        }
        if dispatch.pause_dialogue {
            self.pause_dialogue();
        }
        self.finish_input(dispatch.handled)
    }
    /// Submit an accessibility action from the host through accepted UI geometry.
    pub fn handle_access(
        &mut self,
        bridge: &mut PresentationBridge,
        event: reel_ui::accesskit::ActionRequest,
    ) -> Result<UiDispatch, Error> {
        self.binding.borrow_mut().non_pointer = true;
        let handled = bridge.handle_access_event(event);
        self.binding.borrow_mut().non_pointer = false;
        self.finish_input(handled)
    }
    pub(super) fn finish_input(&mut self, handled: Handled) -> Result<UiDispatch, Error> {
        if let Some(UiInput::Session { owner, action }) = self.inputs.borrow_mut().take() {
            // Owner/session cancellation may race the final UI dispatch. Host
            // rejections filter that event; they are not window failures.
            // Assign the new event at final dispatch, after any external input
            // through session_mut. Already-sequenced replays use handle_input.
            let advance = matches!(&action, InputAction::Advance(_));
            match self.session.handle_local_input(owner, action) {
                Err(InputRejection::SequenceExhausted) => {
                    return Err("input sequence exhausted".into());
                }
                Ok(_) if advance => {
                    self.session.set_skip_reading(false, false, false);
                    self.session.set_auto_reading(false, None);
                }
                _ => {}
            }
        }
        let events = self.take_events();
        if events.iter().any(|event| {
            matches!(
                event,
                reel_ui::stage::NodeEvent::Scroll(_)
                    | reel_ui::stage::NodeEvent::Change(_)
                    | reel_ui::stage::NodeEvent::Text(_)
            )
        }) {
            self.image_demand_dirty = self.pending_stage.is_some() || self.stage.is_some();
            self.image_layout_pending = false;
        }
        if events.iter().any(|event| {
            matches!(
                event,
                reel_ui::stage::NodeEvent::Change(_) | reel_ui::stage::NodeEvent::Text(_)
            )
        }) {
            // Restore authoritative control values even after a rejected proposal.
            self.stage_dirty = true;
        }
        Ok(UiDispatch { handled, events })
    }
}
