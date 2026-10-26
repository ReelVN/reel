//! Read-only input state and callbacks share the existing routed node events.
use super::*;
use reel_ui::stage::{InteractionState, NodeInteraction};

pub(super) const FIELDS: [&str; 5] = ["hover", "pressed", "focused", "disabled", "focus_visible"];

pub(super) fn callback_field(interaction_offset: u32) -> u32 {
    interaction_offset - NodeProperty::PROPERTIES.len() as u32
        + NodeProperty::OnInteraction.index() as u32
}

pub(super) fn listening(
    heap: &Heap,
    fields: Value,
    interaction_offset: u32,
    selected: Value,
) -> Result<bool, HeapError> {
    let offset = interaction_offset - NodeProperty::PROPERTIES.len() as u32;
    if heap
        .table_entry(fields, callback_field(interaction_offset))?
        .1
        .tag()
        != ValueTag::Null
    {
        return Ok(true);
    }
    let appearance = overrides::get(
        heap,
        fields,
        offset as usize,
        selected,
        NodeProperty::Appearance.index(),
    )?;
    if appearance.tag() != ValueTag::Null {
        for state in 2..=5 {
            let entries = heap.host_payload(appearance, state)?;
            if entries.tag() != ValueTag::Null && heap.sequence_len(entries)? != 0 {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

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
    pub(in crate::aside) fn interaction_callback(
        &self,
        heap: &Heap,
        node: NodeId,
    ) -> Result<Option<(Value, Value)>, AdapterError> {
        let Some(binding) = self.handles.get(&node) else {
            return Ok(None);
        };
        let target = heap.import_handle(binding.handle)?;
        let callback = heap
            .table_entry(
                heap.host_fields(target)?,
                callback_field(binding.interaction_offset),
            )?
            .1;
        Ok((callback.tag() != ValueTag::Null).then_some((target, callback)))
    }

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
