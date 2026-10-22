//! One accepted-frame event policy for the base scene and floating stage layers.
use super::{InputBinding, RefCell};
use reel_ui::{core::ErasedAction, input::ActionPhase, stage::NodeEvent};

pub(super) fn accept(
    binding: &RefCell<InputBinding>,
    events: &RefCell<Vec<NodeEvent>>,
    action: &ErasedAction,
) -> bool {
    if let Some(selection) = action.downcast_ref::<crate::stage_widget::NavigationSelection>() {
        events.borrow_mut().push(NodeEvent::Change(selection.0));
        return true;
    }
    let Some(event) = action.downcast_ref::<NodeEvent>() else {
        return false;
    };
    admit(binding, events, event);
    true
}

pub(super) fn admit(
    binding: &RefCell<InputBinding>,
    events: &RefCell<Vec<NodeEvent>>,
    event: &NodeEvent,
) {
    match event {
        NodeEvent::Link(link) if binding.borrow().permits_control() => {
            events.borrow_mut().push(NodeEvent::Link(link.clone()));
        }
        NodeEvent::Click(click) if binding.borrow().permits_button(click.node) => {
            events.borrow_mut().push(NodeEvent::Click(*click));
        }
        NodeEvent::PopupDismiss(event) if binding.borrow().permits_control() => {
            events.borrow_mut().push(NodeEvent::PopupDismiss(*event));
        }
        NodeEvent::Change(change) if binding.borrow().permits_control() => {
            events.borrow_mut().push(NodeEvent::Change(*change));
        }
        NodeEvent::Text(input) if binding.borrow().permits_text(&input.event) => {
            events.borrow_mut().push(NodeEvent::Text(input.clone()));
        }
        NodeEvent::Action(action)
            if matches!(action.phase, ActionPhase::Released | ActionPhase::Cancelled)
                || binding.borrow().permits_control() =>
        {
            events.borrow_mut().push(NodeEvent::Action(action.clone()));
        }
        NodeEvent::Key(input)
            if matches!(input.phase, ActionPhase::Released | ActionPhase::Cancelled)
                || binding.borrow().permits_control() =>
        {
            events.borrow_mut().push(NodeEvent::Key(input.clone()));
        }
        NodeEvent::Interaction(state) => {
            events.borrow_mut().push(NodeEvent::Interaction(*state));
        }
        NodeEvent::Scroll(state) => events.borrow_mut().push(NodeEvent::Scroll(*state)),
        NodeEvent::VirtualList(state) => {
            let mut events = events.borrow_mut();
            if let Some(index) = events.iter().position(|event| {
                        matches!(event, NodeEvent::VirtualList(previous) if previous.node == state.node)
                    }) {
                        events.remove(index);
                    }
            // The latest window follows its matching Scroll offset, even
            // when several compose passes are coalesced in one batch.
            events.push(NodeEvent::VirtualList(state.clone()));
        }
        NodeEvent::Pointer(input)
            if matches!(
                input.phase,
                reel_ui::stage::PointerPhase::Released | reel_ui::stage::PointerPhase::Cancelled
            ) || binding.borrow().permits_control() =>
        {
            events.borrow_mut().push(NodeEvent::Pointer(*input))
        }
        NodeEvent::Wheel(input) if binding.borrow().control_accepted.is_some() => {
            events.borrow_mut().push(NodeEvent::Wheel(*input));
        }
        _ => {}
    }
}
