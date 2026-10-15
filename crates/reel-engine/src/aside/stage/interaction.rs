//! Read-only state produced by the shared input router, with ordinary bindings.
use super::*;
use reel_ui::stage::{InteractionState, NodeInteraction};

pub(super) const FIELDS: [&str; 5] = ["hover", "pressed", "focused", "disabled", "focus_visible"];

pub(super) fn publish(
    heap: &mut Heap,
    host: Value,
    offset: u32,
    state: InteractionState,
) -> Result<bool, AdapterError> {
    let mut changed = false;
    for (i, value) in [
        state.hover,
        state.pressed,
        state.focused,
        state.disabled,
        state.focus_visible,
    ]
    .into_iter()
    .enumerate()
    {
        let property = PropertyId {
            host,
            field: offset + i as u32,
        };
        let value = Value::bool(value);
        if !heap.host_property(property)?.same_bits(value) {
            heap.publish_host_property(property, value)?;
            changed = true;
        }
    }
    Ok(changed)
}

impl StageBinding {
    pub(in crate::aside) fn interaction(
        &mut self,
        heap: &mut Heap,
        event: &NodeInteraction,
    ) -> Result<bool, AdapterError> {
        let Some(binding) = self.handles.get(&event.node) else {
            return Ok(false);
        };
        let host = heap.import_handle(binding.handle)?;
        publish(heap, host, binding.interaction_offset, event.state)
    }
}
