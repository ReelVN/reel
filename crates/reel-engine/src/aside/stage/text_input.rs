//! Committed text is written through the same property binding as other controls.
use super::*;
use reel_ui::{stage::NodeText, widgets::TextInputEvent};

#[derive(aside_host::Properties)]
pub(in crate::aside) enum TextInputField {
    #[aside(name = "value", ty = String, default = "", validate = validate_value)]
    Value,
    #[aside(name = "on_change", ty = Any, default = null, validate = validate_callback)]
    OnChange,
    #[aside(name = "on_submit", ty = Any, default = null, validate = validate_callback)]
    OnSubmit,
}

fn validate_value(heap: &Heap, value: Value) -> Result<(), HeapError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String {
        return Err(HeapError::TypeMismatch);
    }
    let mut offset = 0;
    while offset < info.len {
        let bytes = heap.byte_segment(value, offset)?;
        if bytes.iter().any(|c| matches!(c, b'\r' | b'\n')) {
            return Err(HeapError::TypeMismatch);
        }
        offset += bytes.len() as u32;
    }
    Ok(())
}
fn validate_callback(heap: &Heap, value: Value) -> Result<(), HeapError> {
    validate_button_property(heap, 1, value)
}
pub(super) fn validate(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    let count = TextInputField::PROPERTIES.len() as u32;
    if index < count {
        TextInputField::validate(heap, index, value)
    } else {
        validate_layout_property(heap, index - count, value)
    }
}
pub(super) fn kind(heap: &Heap, fields: Value, limit: usize) -> Result<NodeKind, AdapterError> {
    for index in 0..TextInputField::PROPERTIES.len() as u32 {
        TextInputField::validate(heap, index, heap.table_entry(fields, index)?.1)?;
    }
    Ok(NodeKind::TextInput {
        value: read_text(heap, heap.table_entry(fields, 0)?.1, limit)?.into(),
    })
}

impl StageBinding {
    pub(in crate::aside) fn text_input(
        &mut self,
        heap: &mut Heap,
        input: &NodeText,
    ) -> Result<Option<(Value, Value, Value)>, AdapterError> {
        if !self
            .scene
            .node(input.node)
            .is_some_and(|node| matches!(node.kind, NodeKind::TextInput { .. }))
            || !self.mounted_in_scope(heap, input.node, self.modal_scope(heap)?)?
        {
            return Ok(None);
        }
        let target = self.value(heap, input.node)?;
        let fields = heap.host_fields(target)?;
        let current = heap
            .table_entry(fields, TextInputField::Value.index() as u32)?
            .1;
        let (baseline, value, callback) = match &input.event {
            TextInputEvent::Changed { previous, value } => {
                (previous, value, TextInputField::OnChange)
            }
            TextInputEvent::Submitted(value) => (value, value, TextInputField::OnSubmit),
        };
        // An edit queued against an older model must not overwrite an author's replacement.
        if heap.info(current)?.len as usize != baseline.len() {
            return Ok(None);
        }
        let mut offset = 0;
        while offset < baseline.len() {
            let bytes = heap.byte_segment(current, offset as u32)?;
            if baseline.as_bytes()[offset..offset + bytes.len()] != *bytes {
                return Ok(None);
            }
            offset += bytes.len();
        }
        let value = match &input.event {
            TextInputEvent::Changed { .. } => {
                let value = heap.alloc_string(value)?;
                heap.write_host_property(
                    PropertyId {
                        host: target,
                        field: TextInputField::Value.index() as u32,
                    },
                    value,
                    true,
                )?;
                value
            }
            TextInputEvent::Submitted(_) => current,
        };
        Ok(Some((
            target,
            heap.table_entry(fields, callback.index() as u32)?.1,
            value,
        )))
    }
}
