use super::*;
use aside_host::FromValue;
use reel_ui::{
    input::{Binding, Control},
    stage::{ActionDefinition, InputBindings, NodeAction},
};
use std::sync::Arc;

pub(super) fn read_bindings(heap: &Heap, value: Value) -> Result<InputBindings, AdapterError> {
    let mut definitions = Vec::new();
    for i in 0..heap.table_len(value)? {
        let (name, values) = heap.table_entry(value, i)?;
        let name: Arc<str> = String::from_value(heap, name)?.into();
        let mut bindings = Vec::new();
        for j in 0..heap.sequence_len(values)? {
            let item = heap.sequence_get(values, j)?;
            if heap.sequence_len(item)? != 3 {
                return Err(AdapterError::Input(
                    "expected binding(control, scale:, dead_zone:)",
                ));
            }
            let name = String::from_value(heap, heap.sequence_get(item, 0)?)?;
            let control: Control = name
                .parse()
                .map_err(|_| AdapterError::Input("unknown physical control"))?;
            bindings.push(Binding {
                control,
                scale: number(heap.sequence_get(item, 1)?)? as f32,
                dead_zone: number(heap.sequence_get(item, 2)?)? as f32,
                positive_only: false,
            });
        }
        definitions.push(ActionDefinition { name, bindings });
    }
    InputBindings::new(definitions)
        .map_err(|_| AdapterError::Input("invalid action name, scale or dead zone"))
}
pub(super) fn validate(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    match index {
        0 => read_bindings(heap, value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        1 => validate_button_property(heap, 1, value),
        _ => validate_layout_property(heap, index - 2, value),
    }
}

impl StageBinding {
    pub(in crate::aside) fn input_action(
        &mut self,
        heap: &Heap,
        action: &NodeAction,
    ) -> Result<Option<(Value, Value)>, AdapterError> {
        use reel_ui::input::ActionPhase;
        let Some(node) = self.scene.node(action.node) else {
            return Ok(None);
        };
        let NodeKind::Input { bindings, .. } = &node.kind else {
            return Ok(None);
        };
        let terminal = matches!(action.phase, ActionPhase::Released | ActionPhase::Cancelled);
        if *bindings != action.bindings && action.phase != ActionPhase::Cancelled {
            return Ok(None);
        }
        let target = self.value(heap, action.node)?;
        // Terminal events clean old ownership even when visibility or modal scope changed.
        // The UI already routed against accepted modal geometry. Recheck this
        // node's current ancestry/flags without rebuilding the entire scope per axis update.
        if !terminal && !self.mounted_in_scope(heap, action.node, None)? {
            return Ok(None);
        }
        if *bindings == action.bindings {
            let values = &mut self.handles.get_mut(&action.node).unwrap().input_values;
            if let Some(value) = values.get_mut(&action.action) {
                *value = action.value;
            }
        }
        let closure = heap.table_entry(heap.host_fields(target)?, 1)?.1;
        Ok((closure.tag() != ValueTag::Null).then_some((target, closure)))
    }
}

#[aside_host::native(symbol = "reel::binding", effects = Effects(Effects::ALLOCATE | Effects::HOST), capabilities = ["reel.stage"])]
pub(crate) fn binding(
    #[aside(context)] context: &mut CallContext<'_, StageBinding>,
    control: String,
    #[aside(default = 1)] scale: Value,
    #[aside(default = 0)] dead_zone: Value,
) -> Result<Value, AdapterError> {
    let parsed: Control = control
        .parse()
        .map_err(|_| AdapterError::Input("unknown physical control"))?;
    let scale = number(scale)? as f32;
    let dead_zone = number(dead_zone)? as f32;
    reel_ui::input::ActionMap::new([(
        (),
        vec![Binding {
            control: parsed,
            scale,
            dead_zone,
            positive_only: false,
        }],
    )])
    .map_err(|_| AdapterError::Input("invalid binding scale or dead zone"))?;
    let heap = context.vm.heap_mut();
    let control = heap.alloc_string(&control)?;
    Ok(heap.alloc_array(&[
        control,
        Value::float(scale.into()),
        Value::float(dead_zone.into()),
    ])?)
}

#[aside_host::native(symbol = "reel::node::input_value", effects = Effects(Effects::HOST), capabilities = ["reel.stage"])]
pub(crate) fn value(
    #[aside(context)] context: &mut CallContext<'_, StageBinding>,
    receiver: Value,
    action: String,
) -> Result<f64, AdapterError> {
    let id = context.host.id(context.vm.heap(), receiver)?;
    // A synchronous read after bindings assignment observes the new neutral state.
    let heap = context.vm.heap();
    let source = heap.table_entry(heap.host_fields(receiver)?, 0)?.1;
    if context.host.handles[&id].input_source != Some(heap.handle(source)?) {
        for i in 0..heap.table_len(source)? {
            if String::from_value(heap, heap.table_entry(source, i)?.0)? == action {
                return Ok(0.);
            }
        }
        return Err(AdapterError::Input("unknown input action"));
    }
    let value = context.host.handles[&id]
        .input_values
        .get(action.as_str())
        .ok_or(AdapterError::Input("unknown input action"))?;
    Ok((*value).into())
}
