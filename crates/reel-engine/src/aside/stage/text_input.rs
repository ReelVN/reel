//! Committed text is written through the same property binding as other controls.
use super::*;
use reel_ui::{
    stage::{NodeText, TextInputOptions},
    widgets::{TextInputEvent, TextSelection},
};
pub(in crate::aside) mod filter;
mod history;
mod queue;
mod runtime;
mod saved;
pub(super) use history::{History, HistoryState};
pub(super) use queue::project as project_queue;
pub(super) use runtime::describe;
pub(in crate::aside) use runtime::{contracts, dispatch, documentation};
pub(in crate::aside) use saved::{SavedTextInputIntent, snapshot_intents, snapshot_records};

pub(super) const METRICS: [&str; 4] = [
    "selection_anchor",
    "selection_focus",
    "can_undo",
    "can_redo",
];
pub(super) fn metrics() -> u32 {
    (TextInputField::PROPERTIES.len() + NodeProperty::PROPERTIES.len() + interaction::FIELDS.len())
        as u32
}
pub(super) fn selection(heap: &Heap, host: Value) -> Result<TextSelection, HeapError> {
    Ok(TextSelection {
        anchor: heap
            .host_property(PropertyId {
                host,
                field: metrics(),
            })?
            .as_int()
            .unwrap() as usize,
        focus: heap
            .host_property(PropertyId {
                host,
                field: metrics() + 1,
            })?
            .as_int()
            .unwrap() as usize,
    })
}
pub(super) fn revision(heap: &Heap, host: Value) -> Result<u64, HeapError> {
    Ok(heap.host_payload(host, 4)?.as_int().unwrap() as u64)
}
pub(super) fn project(heap: &Heap, host: Value, kind: &mut NodeKind) -> Result<(), HeapError> {
    if let NodeKind::TextInput { options, .. } = kind {
        options.selection = selection(heap, host)?;
        options.selection_revision = revision(heap, host)?;
    }
    Ok(())
}

pub(super) fn publish(
    heap: &mut Heap,
    host: Value,
    value: Option<Value>,
    selection: TextSelection,
    undo: bool,
    redo: bool,
) -> Result<(), HeapError> {
    let field = metrics();
    let mut writes = [
        (
            PropertyId { host, field },
            Value::int(selection.anchor as i64),
        ),
        (
            PropertyId {
                host,
                field: field + 1,
            },
            Value::int(selection.focus as i64),
        ),
        (
            PropertyId {
                host,
                field: field + 2,
            },
            Value::bool(undo),
        ),
        (
            PropertyId {
                host,
                field: field + 3,
            },
            Value::bool(redo),
        ),
        (
            PropertyId {
                host,
                field: TextInputField::Value.index() as u32,
            },
            Value::NULL,
        ),
    ];
    if let Some(value) = value {
        writes[4].1 = value;
        heap.publish_host_properties(&writes)
    } else {
        heap.publish_host_properties(&[writes[0], writes[1], writes[2], writes[3]])
    }
}

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
    /// Filter one user replacement before publishing value, selection and undo history.
    /// Receive a readonly TextEdit with previous/value/text and start/end UTF-8 byte
    /// offsets into previous. Return replacement text, or null to reject the edit.
    /// Returning the same text preserves selection; transformed text places the caret
    /// after its replacement. IME preedit, undo/redo and script assignments bypass it.
    /// Filtering may yield to the execution budget, but cannot wait. Faults commit
    /// nothing. on_change runs once after an accepted transaction.
    #[aside(name = "edit_filter", ty = Any, doc_type = filter::source_type(), default = null, validate = validate_callback)]
    EditFilter,

    /// Display one bullet per Unicode grapheme, including IME preedit. The value,
    /// selection and undo history retain source UTF-8; switching preserves them.
    /// Copy exports no text, cut only deletes, and accessibility hides the original text.
    #[aside(name = "password", ty = Bool, default = false)]
    Password,
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
    if index == count + NodeProperty::TextGradient.index() as u32 && value.tag() != ValueTag::Null {
        return Err(HeapError::TypeMismatch);
    }
    if (metrics()..metrics() + 4).contains(&index) {
        if index < metrics() + 2 {
            if value.as_int().is_some_and(|n| n >= 0) {
                Ok(())
            } else {
                Err(HeapError::TypeMismatch)
            }
        } else if value.as_bool().is_some() {
            Ok(())
        } else {
            Err(HeapError::TypeMismatch)
        }
    } else if multiline && index == TextInputField::Value.index() as u32 {
        if heap.info(value)?.kind == ObjectKind::String {
            Ok(())
        } else {
            Err(HeapError::TypeMismatch)
        }
    } else if index < count {
        TextInputField::validate(heap, index, value)
    } else {
        if index - count == NodeProperty::WritingMode.index() as u32
            && super::writing_mode(heap, value)?.is_vertical()
        {
            return Err(HeapError::TypeMismatch);
        }
        validate_layout_property(heap, index - count, value)
    }
}
pub(super) fn kind(
    heap: &Heap,
    fields: Value,
    limit: usize,
    multiline: bool,
) -> Result<NodeKind, AdapterError> {
    let gradient = heap
        .table_entry(
            fields,
            (TextInputField::PROPERTIES.len() + NodeProperty::TextGradient.index()) as u32,
        )?
        .1;
    if gradient.tag() != ValueTag::Null {
        return Err(AdapterError::Input(
            "text_gradient applies to text and control captions",
        ));
    }
    for index in 0..TextInputField::PROPERTIES.len() as u32 {
        validate(heap, index, heap.table_entry(fields, index)?.1, multiline)?;
    }
    Ok(NodeKind::TextInput {
        value: read_text(heap, heap.table_entry(fields, 0)?.1, limit)?.into(),
        multiline,
        options: options(heap, fields, None, limit)?,
    })
}

pub(super) fn matches_text(heap: &Heap, value: Value, expected: &str) -> Result<bool, HeapError> {
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
        submit_listening: get(TextInputField::OnSubmit)?.tag() != ValueTag::Null,
        password: boolean(get(TextInputField::Password)?)?,
        selection_color: color(heap, get(TextInputField::SelectionColor)?)?,
        caret_color: if caret.tag() == ValueTag::Null {
            None
        } else {
            Some(color(heap, caret)?)
        },
        selection: Default::default(),
        selection_revision: 0,
        pending_edit: false,
        filter_edits: get(TextInputField::EditFilter)?.tag() != ValueTag::Null,
        replay: None,
    })
}

impl StageBinding {
    #[cfg(test)]
    pub(in crate::aside) fn text_history_entries(
        &self,
        node: reel_ui::stage::NodeId,
    ) -> Option<usize> {
        self.handles
            .get(&node)?
            .text_history
            .as_ref()
            .map(|h| h.entries.len())
    }
    #[cfg(test)]
    pub(in crate::aside) fn text_queue_budget(
        &self,
        node: reel_ui::stage::NodeId,
    ) -> Option<reel_core::budget::BudgetPool> {
        Some(
            self.handles
                .get(&node)?
                .text_history
                .as_ref()?
                .queue
                .as_ref()?
                .pool
                .clone(),
        )
    }
    pub(in crate::aside) fn text_target_current(
        &self,
        heap: &Heap,
        node: reel_ui::stage::NodeId,
    ) -> Result<bool, AdapterError> {
        Ok(self
            .scene
            .node(node)
            .is_some_and(|n| matches!(n.kind, NodeKind::TextInput { .. }))
            && self.mounted_in_scope(heap, node, self.modal_scope(heap)?)?)
    }
    pub(in crate::aside) fn text_action(
        &self,
        heap: &Heap,
        node: reel_ui::stage::NodeId,
        value: &str,
        requested: u64,
    ) -> Result<bool, AdapterError> {
        if !self
            .scene
            .node(node)
            .is_some_and(|n| matches!(n.kind, NodeKind::TextInput { .. }))
            || !self.mounted_in_scope(heap, node, self.modal_scope(heap)?)?
        {
            return Ok(false);
        }
        let target = self.value(heap, node)?;
        Ok(revision(heap, target)? == requested
            && heap
                .host_property(PropertyId {
                    host: target,
                    field: TextInputField::ReadOnly.index() as u32,
                })?
                .as_bool()
                == Some(false)
            && matches_text(
                heap,
                heap.host_property(PropertyId {
                    host: target,
                    field: 0,
                })?,
                value,
            )?)
    }
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
        if matches!(
            input.event,
            TextInputEvent::Changed { .. }
                | TextInputEvent::Undo { .. }
                | TextInputEvent::Redo { .. }
        ) && heap
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
        if let TextInputEvent::Selection {
            value,
            selection,
            revision: requested,
            replay,
        } = &input.event
        {
            if *requested != revision(heap, target)? || !matches_text(heap, current, value)? {
                return Ok(None);
            }
            validate_selection(value, *selection)?;
            let history = self
                .handles
                .get_mut(&input.node)
                .unwrap()
                .text_history
                .as_mut();
            let (undo, redo) = history
                .as_ref()
                .map_or((false, false), |h| (h.can_undo(), h.can_redo()));
            publish(heap, target, None, *selection, undo, redo)?;
            if let Some(history) = history {
                history.split();
            }
            if let Some(sequence) = replay {
                self.complete_text_replay(input.node, *sequence);
            }
            return Ok(Some((target, Value::NULL, current)));
        }
        let (baseline, value, callback) = match &input.event {
            TextInputEvent::Changed {
                previous, value, ..
            } => (previous, value, TextInputField::OnChange),
            TextInputEvent::Submitted { value, .. } => (value, value, TextInputField::OnSubmit),
            _ => return Ok(None),
        };
        // An edit queued against an older model must not overwrite an author's replacement.
        if !matches_text(heap, current, baseline)? {
            return Ok(None);
        }
        let value = match &input.event {
            TextInputEvent::Changed { edit, .. } => {
                if edit.revision != revision(heap, target)? {
                    return Ok(None);
                }
                let (removed, inserted) = replacement(baseline, value, edit)?;
                let value = crate::aside::text::alloc_string(heap, value, 1024)?;
                let expected = heap.handle(current)?;
                let binding = self.handles.get_mut(&input.node).unwrap();
                let stale = binding
                    .text_history
                    .as_ref()
                    .is_some_and(|history| history.expected != expected);
                let mut candidate = if binding.text_history.is_none() || stale {
                    Some(History::new(heap, expected)?)
                } else {
                    None
                };
                let history = candidate
                    .as_deref_mut()
                    .unwrap_or_else(|| binding.text_history.as_deref_mut().unwrap());
                let prepared = history.prepare(edit, removed, inserted)?;
                if let Err(error) = publish(heap, target, Some(value), edit.after, true, false) {
                    history.abort(prepared);
                    return Err(error.into());
                }
                history.commit(prepared, edit, removed, inserted, heap.handle(value)?);
                if let Some(candidate) = candidate {
                    binding.text_history = Some(candidate);
                }
                if let Some(sequence) = edit.replay {
                    self.complete_text_replay(input.node, sequence);
                }
                value
            }
            TextInputEvent::Submitted { replay, .. } => {
                if let Some(history) = self
                    .handles
                    .get_mut(&input.node)
                    .unwrap()
                    .text_history
                    .as_mut()
                {
                    history.split();
                }
                if let Some(sequence) = replay {
                    self.complete_text_replay(input.node, *sequence);
                }
                current
            }
            _ => unreachable!("committed text event"),
        };
        Ok(Some((
            target,
            heap.table_entry(fields, callback.index() as u32)?.1,
            value,
        )))
    }
}

fn validate_selection(value: &str, selection: TextSelection) -> Result<(), HeapError> {
    if value.is_char_boundary(selection.anchor) && value.is_char_boundary(selection.focus) {
        Ok(())
    } else {
        Err(HeapError::OutOfBounds)
    }
}

/// Validate the single replacement emitted by the shared editor. Both filtered
/// and immediate publication use these same UTF-8 and proposal checks.
fn replacement<'a>(
    baseline: &'a str,
    value: &'a str,
    edit: &reel_ui::widgets::TextEdit,
) -> Result<(&'a str, &'a str), HeapError> {
    let removed = baseline
        .get(edit.range.clone())
        .ok_or(HeapError::OutOfBounds)?;
    let inserted_end = value
        .len()
        .checked_sub(baseline.len() - edit.range.end)
        .ok_or(HeapError::OutOfBounds)?;
    let inserted = value
        .get(edit.range.start..inserted_end)
        .ok_or(HeapError::OutOfBounds)?;
    if baseline[..edit.range.start] != value[..edit.range.start]
        || baseline[edit.range.end..] != value[inserted_end..]
    {
        return Err(HeapError::TypeMismatch);
    }
    validate_selection(baseline, edit.before)?;
    validate_selection(value, edit.after)?;
    Ok((removed, inserted))
}
