//! Committed text is written through the same property binding as other controls.
use super::*;
use reel_ui::{
    stage::{NodeText, TextInputOptions},
    widgets::TextInputEvent,
};

#[derive(aside_host::Properties)]
pub(in crate::aside) enum TextInputField {
    #[aside(name = "value", ty = String, default = "", validate = validate_value)]
    Value,
    #[aside(name = "on_change", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::Closure), default = null, validate = validate_callback)]
    OnChange,
    #[aside(name = "on_submit", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::Closure), default = null, validate = validate_callback)]
    OnSubmit,
    /// Empty-value hint; hidden during IME composition and never written to value.
    #[aside(name = "placeholder", ty = String, default = "")]
    Placeholder,
    /// Preserve focus, selection, copying and submission while preventing user edits.
    /// Scripted value assignment remains available; enabling cancels active preedit.
    #[aside(name = "read_only", ty = Bool, default = false)]
    ReadOnly,
    /// Selection background in #rrggbb or #rrggbbaa form. Changes only affect paint.
    #[aside(name = "selection_color", ty = String, default = "#3c78dc5f", validate = validate_color)]
    SelectionColor,
    /// Caret color in #rrggbb or #rrggbbaa form. Null follows the common color.
    #[aside(name = "caret_color", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::String), default = null, validate = validate_background)]
    CaretColor,
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
pub(super) fn validate(
    heap: &Heap,
    index: u32,
    value: Value,
    multiline: bool,
) -> Result<(), HeapError> {
    let count = TextInputField::PROPERTIES.len() as u32;
    if multiline && index == TextInputField::Value.index() as u32 {
        if heap.info(value)?.kind == ObjectKind::String {
            Ok(())
        } else {
            Err(HeapError::TypeMismatch)
        }
    } else if index < count {
        TextInputField::validate(heap, index, value)
    } else {
        validate_layout_property(heap, index - count, value)
    }
}
pub(super) fn kind(
    heap: &Heap,
    fields: Value,
    limit: usize,
    multiline: bool,
) -> Result<NodeKind, AdapterError> {
    for index in 0..TextInputField::PROPERTIES.len() as u32 {
        validate(heap, index, heap.table_entry(fields, index)?.1, multiline)?;
    }
    Ok(NodeKind::TextInput {
        value: read_text(heap, heap.table_entry(fields, 0)?.1, limit)?.into(),
        multiline,
        options: options(heap, fields, None, limit)?,
    })
}

fn matches_text(heap: &Heap, value: Value, expected: &str) -> Result<bool, HeapError> {
    if heap.info(value)?.len as usize != expected.len() {
        return Ok(false);
    }
    let mut offset = 0;
    while offset < expected.len() {
        let bytes = heap.byte_segment(value, offset as u32)?;
        if expected.as_bytes()[offset..offset + bytes.len()] != *bytes {
            return Ok(false);
        }
        offset += bytes.len();
    }
    Ok(true)
}

pub(super) fn options(
    heap: &Heap,
    fields: Value,
    previous: Option<&TextInputOptions>,
    limit: usize,
) -> Result<TextInputOptions, AdapterError> {
    let get = |field: TextInputField| -> Result<Value, HeapError> {
        Ok(heap.table_entry(fields, field.index() as u32)?.1)
    };
    let hint = get(TextInputField::Placeholder)?;
    // An unchanged hint shares the existing Arc instead of copying text on every sync.
    let placeholder = if heap.info(hint)?.len == 0 {
        None
    } else if let Some(old) = previous.and_then(|old| old.placeholder.as_ref())
        && matches_text(heap, hint, old)?
    {
        Some(old.clone())
    } else {
        Some(read_text(heap, hint, limit)?.into())
    };
    let caret = get(TextInputField::CaretColor)?;
    Ok(TextInputOptions {
        placeholder,
        read_only: boolean(get(TextInputField::ReadOnly)?)?,
        selection_color: color(heap, get(TextInputField::SelectionColor)?)?,
        caret_color: if caret.tag() == ValueTag::Null {
            None
        } else {
            Some(color(heap, caret)?)
        },
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
        if matches!(input.event, TextInputEvent::Changed { .. })
            && heap
                .table_entry(fields, TextInputField::ReadOnly.index() as u32)?
                .1
                .as_bool()
                == Some(true)
        {
            return Ok(None);
        }
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
        if !matches_text(heap, current, baseline)? {
            return Ok(None);
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
