//! Stateful controls share the stage's property storage and callback ownership.
use super::*;
use reel_ui::{
    stage::{ControlValue, NodeChange, ToggleAppearance},
    widgets::SliderRange,
};

#[derive(aside_host::Properties)]
pub(in crate::aside) enum SliderField {
    #[aside(name = "value", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_number)]
    Value,
    #[aside(name = "min", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_number)]
    Min,
    #[aside(name = "max", ty = Any, doc_type = aside_host::source::number(), default = 1, validate = validate_number)]
    Max,
    #[aside(name = "step", ty = Any, doc_type = aside_host::source::nullable(aside_host::source::number()), default = null, validate = validate_step)]
    Step,
    #[aside(name = "on_change", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::Closure), default = null, validate = validate_callback)]
    OnChange,
}
#[derive(aside_host::Properties)]
pub(in crate::aside) enum ToggleField {
    #[aside(name = "label", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::String), default = null, validate = validate_label)]
    Label,
    #[aside(name = "value", ty = Bool, default = false)]
    Value,
    #[aside(name = "on_change", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::Closure), default = null, validate = validate_callback)]
    OnChange,
}
#[derive(aside_host::Properties)]
pub(in crate::aside) enum RadioField {
    #[aside(name = "option", ty = Any)]
    Option,
    #[aside(name = "label", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::String), default = null, validate = validate_label)]
    Label,
    #[aside(name = "value", ty = Any, default = null)]
    Value,
    #[aside(name = "on_change", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::Closure), default = null, validate = validate_callback)]
    OnChange,
}

pub(super) fn radio_kind(
    heap: &Heap,
    fields: Value,
    revision: u64,
    limit: usize,
) -> Result<NodeKind, AdapterError> {
    let get = |field: RadioField| {
        heap.table_entry(fields, field.index() as u32)
            .map(|(_, v)| v)
    };
    let label = get(RadioField::Label)?;
    validate_callback(heap, get(RadioField::OnChange)?)?;
    Ok(NodeKind::Radio {
        label: if label.tag() == ValueTag::Null {
            None
        } else {
            Some(read_text(heap, label, limit)?)
        },
        selected: super::super::keys::compare(
            heap,
            get(RadioField::Value)?,
            get(RadioField::Option)?,
            limit,
        )?
        .is_eq(),
        group: None,
        revision,
    })
}

pub(super) fn project_group(
    heap: &Heap,
    value: Value,
    kind: &mut NodeKind,
) -> Result<(), HeapError> {
    if let NodeKind::Radio { group, .. } = kind {
        let (source, writable) = heap.property_source(PropertyId {
            host: value,
            field: RadioField::Value.index() as u32,
        })?;
        *group = writable.then_some(reel_ui::core::SelectionGroupId(
            heap.handle(source.host)?.key,
            source.field,
        ));
    }
    Ok(())
}

pub(super) fn validate_radio(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    let count = RadioField::PROPERTIES.len() as u32;
    if index < count {
        RadioField::validate(heap, index, value)
    } else {
        validate_layout_property(heap, index - count, value)
    }
}
fn validate_step(heap: &Heap, value: Value) -> Result<(), HeapError> {
    if value.tag() == ValueTag::Null {
        return Ok(());
    }
    validate_number(heap, value)?;
    if number(value).map_err(|_| HeapError::TypeMismatch)? <= 0. {
        return Err(HeapError::TypeMismatch);
    }
    Ok(())
}
fn validate_callback(heap: &Heap, value: Value) -> Result<(), HeapError> {
    validate_button_property(heap, 1, value)
}
fn validate_label(heap: &Heap, value: Value) -> Result<(), HeapError> {
    validate_button_property(heap, 0, value)
}
fn slider_values(
    heap: &aside_vm::PropertyRead<'_>,
    fields: Value,
    replacement: Option<(u32, Value)>,
) -> Result<(SliderRange, f64), AdapterError> {
    let get = |field: SliderField| -> Result<Value, HeapError> {
        let index = field.index() as u32;
        if let Some((at, value)) = replacement
            && at == index
        {
            return Ok(value);
        }
        Ok(heap.table_entry(fields, index)?.1)
    };
    let step = get(SliderField::Step)?;
    let step = if step.tag() == ValueTag::Null {
        None
    } else {
        Some(number(step)?)
    };
    let range = SliderRange::new(
        number(get(SliderField::Min)?)?,
        number(get(SliderField::Max)?)?,
        step,
    )
    .map_err(|_| {
        AdapterError::Input("slider requires a finite increasing range and positive optional step")
    })?;
    let value = number(get(SliderField::Value)?)?;
    if !range.contains(value) {
        return Err(AdapterError::Input("slider value must be within its range"));
    }
    Ok((range, value))
}
pub(super) fn slider_kind(heap: &Heap, fields: Value) -> Result<NodeKind, AdapterError> {
    let (range, value) = slider_values(&aside_vm::PropertyRead::current(heap), fields, None)?;
    let callback = heap
        .table_entry(fields, SliderField::OnChange.index() as u32)?
        .1;
    validate_callback(heap, callback)?;
    Ok(NodeKind::Slider {
        range,
        value,
        listening: callback.tag() != ValueTag::Null,
    })
}
pub(super) fn toggle_kind(
    heap: &Heap,
    fields: Value,
    limit: usize,
    appearance: ToggleAppearance,
) -> Result<NodeKind, AdapterError> {
    let label = heap
        .table_entry(fields, ToggleField::Label.index() as u32)?
        .1;
    let label = if label.tag() == ValueTag::Null {
        None
    } else {
        Some(read_text(heap, label, limit)?)
    };
    let value = heap
        .table_entry(fields, ToggleField::Value.index() as u32)?
        .1
        .as_bool()
        .ok_or(AdapterError::Input("toggle value must be boolean"))?;
    let callback = heap
        .table_entry(fields, ToggleField::OnChange.index() as u32)?
        .1;
    validate_callback(heap, callback)?;
    Ok(NodeKind::Toggle {
        label,
        value,
        listening: callback.tag() != ValueTag::Null,
        appearance,
    })
}
pub(super) fn validate_slider(
    heap: &aside_vm::PropertyRead<'_>,
    host: Value,
    index: u32,
    value: Value,
) -> Result<(), HeapError> {
    let count = SliderField::PROPERTIES.len() as u32;
    if index >= count {
        return validate_layout_property(heap, index - count, value);
    }
    SliderField::validate(heap, index, value)?;
    validation(slider_values(
        heap,
        heap.host_fields(host)?,
        Some((index, value)),
    ))
}
pub(super) fn validate_toggle(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    let count = ToggleField::PROPERTIES.len() as u32;
    if index < count {
        ToggleField::validate(heap, index, value)
    } else {
        validate_layout_property(heap, index - count, value)
    }
}
impl StageBinding {
    pub(in crate::aside) fn change(
        &mut self,
        heap: &mut Heap,
        change: &NodeChange,
    ) -> Result<Option<(Value, Value, Value)>, AdapterError> {
        let Some(node) = self.scene.node(change.node) else {
            return Ok(None);
        };
        let slider = matches!(node.kind, NodeKind::Slider { .. });
        let radio = matches!(node.kind, NodeKind::Radio { .. });
        if !slider && !radio && !matches!(node.kind, NodeKind::Toggle { .. }) {
            return Ok(None);
        }
        if !self.mounted_in_scope(heap, change.node, self.modal_scope(heap)?)? {
            return Ok(None);
        }
        let target = self.value(heap, change.node)?;
        let fields = heap.host_fields(target)?;
        let (index, handler, value) = if slider {
            let (range, _) = slider_values(&aside_vm::PropertyRead::current(heap), fields, None)?;
            let ControlValue::Number(value) = change.value else {
                return Ok(None);
            };
            if change.range != Some(range) || !range.contains(value) {
                return Ok(None);
            }
            (
                SliderField::Value.index(),
                SliderField::OnChange.index(),
                Value::float(range.snap(value)),
            )
        } else if let NodeKind::Radio { revision, .. } = node.kind {
            if change.value != ControlValue::Selection(revision) {
                return Ok(None);
            }
            let option = heap
                .table_entry(fields, RadioField::Option.index() as u32)?
                .1;
            // A script can change option after presentation but before stage sync.
            if !self.handles[&change.node]
                .radio_option
                .is_some_and(|old| old.same_bits(option))
            {
                return Ok(None);
            }
            (
                RadioField::Value.index(),
                RadioField::OnChange.index(),
                option,
            )
        } else {
            let ControlValue::Bool(value) = change.value else {
                return Ok(None);
            };
            (
                ToggleField::Value.index(),
                ToggleField::OnChange.index(),
                Value::bool(value),
            )
        };
        let (_, old) = heap.table_entry(fields, index as u32)?;
        let equal = if slider {
            number(old)? == number(value)?
        } else if radio {
            super::super::keys::compare(heap, old, value, 256)?.is_eq()
        } else {
            old.same_bits(value)
        };
        if equal {
            return Ok(None);
        }
        match heap.write_host_property(
            PropertyId {
                host: target,
                field: index as u32,
            },
            value,
            true,
        ) {
            Ok(()) => {}
            Err(HeapError::TypeMismatch | HeapError::ReadOnly | HeapError::RetiredHost) => {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        }
        let node = self.scene.node(change.node).unwrap();
        let mut kind = node.kind.clone();
        match &mut kind {
            NodeKind::Slider { value: current, .. } => *current = number(value)?,
            NodeKind::Toggle { value: current, .. } => *current = value.as_bool().unwrap(),
            NodeKind::Radio { selected, .. } => *selected = true,
            _ => unreachable!(),
        }
        self.scene.update(change.node, kind, node.props.clone())?;
        let closure = heap.table_entry(fields, handler as u32)?.1;
        Ok(Some((target, closure, value)))
    }
}
