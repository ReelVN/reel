//! Read-only input state and callbacks share the existing routed node events.
use super::*;
use reel_ui::stage::{InteractionState, NodeInteraction};

#[derive(Clone, Copy, aside_host::Properties)]
pub(super) enum Field {
    /// Whether this node or a descendant is under the pointer.
    Hover,
    /// Whether pointer capture or the node's confirmation control is pressed.
    Pressed,
    /// Whether this node or a descendant receives keyboard focus.
    Focused,
    /// Whether this node is disabled by itself or an ancestor.
    Disabled,
    /// Whether keyboard or gamepad navigation focus should be displayed.
    #[aside(name = "focus_visible")]
    FocusVisible,
}

#[derive(Clone, Copy, aside_host::Properties)]
pub(super) enum VisualState {
    Base,
    Hover,
    #[aside(name = "focus_visible")]
    FocusVisible,
    Pressed,
    Disabled,
    Highlight,
}

impl VisualState {
    pub(super) const PRIORITY: [Self; 6] = [
        Self::Disabled,
        Self::Pressed,
        Self::FocusVisible,
        Self::Hover,
        Self::Highlight,
        Self::Base,
    ];

    pub(super) fn active(
        self,
        read: impl Fn(Field) -> Result<bool, HeapError>,
    ) -> Result<bool, HeapError> {
        match self {
            Self::Base => Ok(true),
            Self::Disabled => read(Field::Disabled),
            Self::Pressed => read(Field::Pressed),
            Self::FocusVisible => read(Field::FocusVisible),
            Self::Hover => read(Field::Hover),
            Self::Highlight => Ok(read(Field::Hover)? || read(Field::FocusVisible)?),
        }
    }
}

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
        for state in VisualState::PRIORITY
            .into_iter()
            .take_while(|state| !matches!(state, VisualState::Base))
        {
            let entries = heap.host_payload(appearance, 1 + state.index() as u32)?;
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
    for (field, value) in [
        (Field::Hover, state.hover),
        (Field::Pressed, state.pressed),
        (Field::Focused, state.focused),
        (Field::Disabled, state.disabled),
        (Field::FocusVisible, state.focus_visible),
    ] {
        let property = PropertyId {
            host,
            field: offset + field.index() as u32,
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
        let changed = publish(heap, host, binding.interaction_offset, event.state)?;
        if changed {
            self.sync_tooltips(heap, event.node)?;
        }
        Ok(changed)
    }
}
