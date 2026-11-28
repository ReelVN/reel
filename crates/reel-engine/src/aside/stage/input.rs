use super::*;
use aside_host::FromValue;
use reel_ui::{
    input::{ActionPhase, Binding, Control},
    stage::{ActionDefinition, InputBindings, NodeAction, NodeKey},
};
use std::sync::Arc;

#[derive(aside_host::Properties)]
pub(in crate::aside) enum InputField {
    /// Named action records with required `bindings` and optional `on_press`,
    /// `on_change`, `on_release`, `on_cancel`, or exclusive `on_action`.
    /// Plain control strings use scale 1 and dead_zone 0; configured controls use
    /// `binding`. Construction installs immediately; an assigned replacement
    /// table installs bindings and handlers together at stage synchronization.
    /// Once installed, internal edits do not update the map: assign a new table.
    /// Unhandled phases
    /// update held values without creating callback tasks or event objects.
    #[aside(name = "actions", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::Table), default = null, validate = validate_actions)]
    Actions,
    /// Receive `target`, physical `code`, logical `key`, and modifier flags
    /// `shift`, `ctrl`, `alt`, `meta`. The `phase` is `pressed`, `repeated`,
    /// `released` or `cancelled`.
    #[aside(name = "on_key", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::Closure), default = null, validate = validate_callback)]
    OnKey,
    /// Receive `target`, local `x`/`y`, and `phase` (`pressed`, `moved`,
    /// `released`, or `cancelled`). Movement follows accepted hit geometry;
    /// primary-button and touch drags stay captured until release or cancellation.
    /// Hover movement leaves focus and pointer capture unchanged.
    /// `dragging` is true during this node's primary-button or touch drag,
    /// including `pressed`, and false for hover, release, and cancellation.
    #[aside(name = "on_pointer", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::Closure), default = null, validate = validate_callback)]
    OnPointer,
    /// Receive wheel input with `target`, local `x` and `y`, and `delta_x` and `delta_y`.
    #[aside(name = "on_wheel", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::Closure), default = null, validate = validate_callback)]
    OnWheel,
    /// Handle the release of Escape, PointerSecondary or GamepadBack.
    /// An explicit `actions.back` supplies its own bindings and handlers.
    #[aside(name = "on_back", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::Closure), default = null, validate = validate_callback)]
    OnBack,
}
fn validate_actions(heap: &Heap, value: Value) -> Result<(), HeapError> {
    validate_actions_view(&aside_vm::PropertyRead::current(heap), value)
}
fn validate_actions_view(heap: &aside_vm::PropertyRead<'_>, value: Value) -> Result<(), HeapError> {
    heap.charge_property_validation_work(4)?;
    let validate = || -> Result<(), AdapterError> {
        if value.tag() == ValueTag::Null {
            return Ok(());
        }
        for index in 0..heap.table_len(value)? {
            heap.charge_property_validation_work(8)?;
            let (name, record) = heap.table_entry(value, index)?;
            let row = read_action(heap, name, record)?;
            for index in 0..heap.sequence_len(row[1])? {
                read_control(heap, heap.sequence_get(row[1], index)?)?;
            }
        }
        Ok(())
    };
    validation(validate())
}
fn validate_callback(heap: &Heap, value: Value) -> Result<(), HeapError> {
    validate_button_property(heap, 1, value)
}

const HANDLERS: [&str; 5] = [
    "on_action",
    "on_press",
    "on_change",
    "on_release",
    "on_cancel",
];
// Input-only payload: [installed declaration, retiring declaration]. Each
// declaration is an array of [name, bindings, full, press, change, release, cancel].
const NODE_SLOT: u32 = 3;
const CURRENT: u32 = 0;
const RETIRING: u32 = 1;
type Row = [Value; 7];
const BACK_CONTROLS: [&str; 3] = ["Escape", "PointerSecondary", "GamepadBack"];

#[derive(Clone, Copy)]
struct Source {
    actions: Value,
    back: Value,
}

impl Source {
    fn read(heap: &Heap, fields: Value) -> Result<Self, HeapError> {
        Ok(Self {
            actions: heap
                .table_entry(fields, InputField::Actions.index() as u32)?
                .1,
            back: heap
                .table_entry(fields, InputField::OnBack.index() as u32)?
                .1,
        })
    }

    fn changed(self, current: Self) -> bool {
        !self.actions.same_bits(current.actions) || !self.back.same_bits(current.back)
    }
}

pub(super) struct Declaration {
    bindings: InputBindings,
    rows: Vec<Row>,
}
struct ActionValue {
    index: u32,
    value: f32,
}
pub(super) struct State {
    source: Source,
    values: BTreeMap<Arc<str>, ActionValue>,
    retiring: Option<Retiring>,
}
struct Retiring {
    bindings: InputBindings,
    held: BTreeMap<Arc<str>, u32>,
}
pub(in crate::aside) struct Callback {
    pub target: Value,
    pub closure: Value,
    pub phase: ActionPhase,
    pub value: f32,
}

fn with_string<T>(
    heap: &Heap,
    value: Value,
    read: impl FnOnce(&str) -> Result<T, AdapterError>,
) -> Result<T, AdapterError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String {
        return Err(HeapError::TypeMismatch.into());
    }
    heap.charge_property_validation_work(u64::from(info.len) + 4)?;
    let bytes = heap.byte_segment(value, 0)?;
    if bytes.len() == info.len as usize {
        read(std::str::from_utf8(bytes).map_err(|_| HeapError::TypeMismatch)?)
    } else {
        read(&String::from_value(heap, value)?)
    }
}

fn read_control(heap: &Heap, item: Value) -> Result<Binding, AdapterError> {
    heap.charge_property_validation_work(8)?;
    let (name, scale, dead_zone) = if heap
        .info(item)
        .is_ok_and(|info| info.kind == ObjectKind::String)
    {
        (item, 1., 0.)
    } else {
        if heap.sequence_len(item)? != 3 {
            return Err(AdapterError::Input(
                "expected a control name or binding(control, scale:, dead_zone:)",
            ));
        }
        (
            heap.sequence_get(item, 0)?,
            number(heap.sequence_get(item, 1)?)? as f32,
            number(heap.sequence_get(item, 2)?)? as f32,
        )
    };
    let control: Control = with_string(heap, name, |name| {
        name.parse()
            .map_err(|_| AdapterError::Input("unknown physical control"))
    })?;
    let binding = Binding {
        control,
        scale,
        dead_zone,
        positive_only: false,
    };
    if !binding.is_valid() {
        return Err(AdapterError::Input("invalid binding scale or dead zone"));
    }
    Ok(binding)
}

fn read_controls(heap: &Heap, values: Value) -> Result<Vec<Binding>, AdapterError> {
    let mut bindings = Vec::new();
    for j in 0..heap.sequence_len(values)? {
        bindings.push(read_control(heap, heap.sequence_get(values, j)?)?);
    }
    Ok(bindings)
}

fn read_action(
    heap: &aside_vm::PropertyRead<'_>,
    name: Value,
    record: Value,
) -> Result<Row, AdapterError> {
    let info = heap.info(name)?;
    if info.kind != ObjectKind::String || info.len == 0 {
        return Err(AdapterError::Input("an action requires a nonempty name"));
    }
    // Table keys already enforce unique string names.
    let mut row = [Value::NULL; 7];
    row[0] = name;
    for j in 0..heap.table_len(record)? {
        heap.charge_property_validation_work(8)?;
        let (field, value) = heap.table_entry(record, j)?;
        let index = with_string(heap, field, |field| {
            if field == "bindings" {
                Ok(1)
            } else {
                HANDLERS
                    .iter()
                    .position(|name| *name == field)
                    .map(|index| 2 + index)
                    .ok_or(AdapterError::Input("unknown action field"))
            }
        })?;
        if index != 1 {
            validate_callback(heap, value)?;
        }
        row[index] = value;
    }
    if row[1].tag() == ValueTag::Null {
        return Err(AdapterError::Input("an action requires bindings"));
    }
    if row[2].tag() != ValueTag::Null && row[3..].iter().any(|v| v.tag() != ValueTag::Null) {
        return Err(AdapterError::Input(
            "on_action and phase handlers are mutually exclusive",
        ));
    }
    Ok(row)
}

fn read_actions(heap: &Heap, source: Source, capture: bool) -> Result<Declaration, AdapterError> {
    let heap = &aside_vm::PropertyRead::current(heap);
    let mut definitions = Vec::new();
    let mut rows = Vec::new();
    if source.actions.tag() != ValueTag::Null {
        for i in 0..heap.table_len(source.actions)? {
            let (key, record) = heap.table_entry(source.actions, i)?;
            let name: Arc<str> = String::from_value(heap, key)?.into();
            let row = read_action(heap, key, record)?;
            definitions.push(ActionDefinition {
                name,
                bindings: read_controls(heap, row[1])?,
            });
            if capture {
                rows.push(row);
            }
        }
    }
    if source.back.tag() != ValueTag::Null && !definitions.iter().any(|a| a.name.as_ref() == "back")
    {
        definitions.push(ActionDefinition {
            name: "back".into(),
            bindings: BACK_CONTROLS
                .into_iter()
                .map(|name| Binding {
                    control: name.parse().expect("registered back control"),
                    scale: 1.,
                    dead_zone: 0.,
                    positive_only: false,
                })
                .collect(),
        });
        if capture {
            let mut row = [Value::NULL; 7];
            row[5] = source.back;
            rows.push(row);
        }
    }
    let bindings = InputBindings::new(definitions)
        .map_err(|_| AdapterError::Input("invalid action name, scale or dead zone"))?;
    Ok(Declaration { bindings, rows })
}

pub(super) fn read_bindings(heap: &Heap, fields: Value) -> Result<InputBindings, AdapterError> {
    Ok(read_actions(heap, Source::read(heap, fields)?, false)?.bindings)
}

fn freeze(heap: &mut Heap, rows: Vec<Row>) -> Result<Value, AdapterError> {
    let snapshot = heap.alloc_array(&[])?;
    for mut row in rows {
        let controls = heap.alloc_array(&[])?;
        if row[0].tag() == ValueTag::Null {
            row[0] = heap.alloc_string("back")?;
            for name in BACK_CONTROLS {
                let name = heap.alloc_string(name)?;
                heap.array_push(controls, name)?;
            }
            row[1] = controls;
            let row = heap.alloc_array(&row)?;
            heap.array_push(snapshot, row)?;
            continue;
        }
        for j in 0..heap.sequence_len(row[1])? {
            let item = heap.sequence_get(row[1], j)?;
            let item = if heap
                .info(item)
                .is_ok_and(|info| info.kind == ObjectKind::String)
            {
                item
            } else {
                let fields = [
                    heap.sequence_get(item, 0)?,
                    heap.sequence_get(item, 1)?,
                    heap.sequence_get(item, 2)?,
                ];
                heap.alloc_array(&fields)?
            };
            heap.array_push(controls, item)?;
        }
        row[1] = controls;
        let row = heap.alloc_array(&row)?;
        heap.array_push(snapshot, row)?;
    }
    Ok(snapshot)
}

fn values(bindings: &InputBindings) -> BTreeMap<Arc<str>, ActionValue> {
    bindings
        .iter()
        .enumerate()
        .map(|(i, action)| {
            (
                action.name.clone(),
                ActionValue {
                    index: i as u32,
                    value: 0.,
                },
            )
        })
        .collect()
}

pub(super) fn prepare(heap: &Heap, fields: Value) -> Result<(NodeKind, Declaration), AdapterError> {
    let declaration = read_actions(heap, Source::read(heap, fields)?, true)?;
    Ok((
        kind(heap, fields, declaration.bindings.clone())?,
        declaration,
    ))
}

pub(super) fn initialize(
    heap: &mut Heap,
    node: Value,
    fields: Value,
    declaration: Declaration,
) -> Result<Box<State>, AdapterError> {
    let snapshot = freeze(heap, declaration.rows)?;
    let payload = heap.alloc_array(&[snapshot, Value::NULL])?;
    heap.set_host_payload(node, NODE_SLOT, payload)?;
    Ok(Box::new(State {
        source: Source::read(heap, fields)?,
        values: values(&declaration.bindings),
        retiring: None,
    }))
}

pub(super) fn restore(
    heap: &mut Heap,
    node: Value,
    fields: Value,
) -> Result<(NodeKind, Box<State>), AdapterError> {
    let payload = heap.host_payload(node, NODE_SLOT)?;
    let snapshot = heap.sequence_get(payload, CURRENT)?;
    let mut definitions = Vec::new();
    for i in 0..heap.sequence_len(snapshot)? {
        let row = heap.sequence_get(snapshot, i)?;
        definitions.push(ActionDefinition {
            name: String::from_value(heap, heap.sequence_get(row, 0)?)?.into(),
            bindings: read_controls(heap, heap.sequence_get(row, 1)?)?,
        });
    }
    let bindings =
        InputBindings::new(definitions).map_err(|_| AdapterError::Input("saved input bindings"))?;
    heap.set_field(payload, RETIRING, Value::NULL)?;
    let state = Box::new(State {
        source: Source::read(heap, fields)?,
        values: values(&bindings),
        retiring: None,
    });
    Ok((kind(heap, fields, bindings)?, state))
}

pub(super) fn kind(
    heap: &Heap,
    fields: Value,
    bindings: InputBindings,
) -> Result<NodeKind, AdapterError> {
    let view = aside_vm::PropertyRead::current(heap);
    for index in 1..InputField::PROPERTIES.len() as u32 {
        validate(&view, index, view.table_entry(fields, index)?.1)?;
    }
    Ok(NodeKind::Input {
        bindings,
        listening: true,
        keyboard: heap.table_entry(fields, 1)?.1.tag() != ValueTag::Null,
        pointer: heap.table_entry(fields, 2)?.1.tag() != ValueTag::Null,
        wheel: heap.table_entry(fields, 3)?.1.tag() != ValueTag::Null,
    })
}

pub(super) fn replace(
    heap: &mut Heap,
    node: Value,
    fields: Value,
    bindings: &InputBindings,
    state: &mut State,
) -> Result<InputBindings, AdapterError> {
    let source = Source::read(heap, fields)?;
    if !state.source.changed(source) {
        return Ok(bindings.clone());
    }
    let declaration = read_actions(heap, source, true)?;
    let snapshot = freeze(heap, declaration.rows)?;
    let payload = heap.host_payload(node, NODE_SLOT)?;
    let held: BTreeMap<_, _> = state
        .values
        .iter()
        .filter(|(_, value)| value.value != 0.)
        .map(|(name, value)| (name.clone(), value.index))
        .collect();
    if !held.is_empty() {
        // The player drains cancellation before routing the next generation.
        assert!(
            state.retiring.is_none(),
            "input cancellation was not delivered"
        );
        let old = heap.sequence_get(payload, CURRENT)?;
        heap.set_field(payload, RETIRING, old)?;
        state.retiring = Some(Retiring {
            bindings: bindings.clone(),
            held,
        });
    }
    heap.set_field(payload, CURRENT, snapshot)?;
    state.source = source;
    state.values = values(&declaration.bindings);
    Ok(declaration.bindings)
}
pub(super) fn validate(
    heap: &aside_vm::PropertyRead<'_>,
    index: u32,
    value: Value,
) -> Result<(), HeapError> {
    let count = InputField::PROPERTIES.len() as u32;
    if index == InputField::Actions.index() as u32 {
        validate_actions_view(heap, value)
    } else if index < count {
        InputField::validate(heap, index, value)
    } else {
        validate_layout_property(heap, index - count, value)
    }
}

impl StageBinding {
    pub(in crate::aside) fn input_wheel(
        &self,
        heap: &Heap,
        event: &reel_ui::stage::NodeWheel,
    ) -> Result<Option<(Value, Value)>, AdapterError> {
        if !self
            .scene
            .node(event.node)
            .is_some_and(|n| matches!(n.kind, NodeKind::Input { wheel: true, .. }))
            || !self.mounted_in_scope(heap, event.node, self.modal_scope(heap)?)?
        {
            return Ok(None);
        }
        let target = self.value(heap, event.node)?;
        let callback = heap
            .table_entry(
                heap.host_fields(target)?,
                InputField::OnWheel.index() as u32,
            )?
            .1;
        Ok((callback.tag() != ValueTag::Null).then_some((target, callback)))
    }
    pub(in crate::aside) fn input_pointer(
        &self,
        heap: &Heap,
        event: &reel_ui::stage::NodePointer,
    ) -> Result<Option<(Value, Value)>, AdapterError> {
        use reel_ui::stage::PointerPhase;
        if !self
            .scene
            .node(event.node)
            .is_some_and(|n| matches!(n.kind, NodeKind::Input { pointer: true, .. }))
        {
            return Ok(None);
        }
        let terminal = matches!(
            event.phase,
            PointerPhase::Released | PointerPhase::Cancelled
        );
        if !terminal && !self.mounted_in_scope(heap, event.node, self.modal_scope(heap)?)? {
            return Ok(None);
        }
        let target = self.value(heap, event.node)?;
        let callback = heap
            .table_entry(
                heap.host_fields(target)?,
                InputField::OnPointer.index() as u32,
            )?
            .1;
        Ok((callback.tag() != ValueTag::Null).then_some((target, callback)))
    }
    pub(in crate::aside) fn input_key(
        &self,
        heap: &Heap,
        key: &NodeKey,
    ) -> Result<Option<(Value, Value)>, AdapterError> {
        use reel_ui::input::ActionPhase;
        if !self
            .scene
            .node(key.node)
            .is_some_and(|n| matches!(n.kind, NodeKind::Input { .. }))
        {
            return Ok(None);
        }
        let terminal = matches!(key.phase, ActionPhase::Released | ActionPhase::Cancelled);
        if !terminal && !self.mounted_in_scope(heap, key.node, self.modal_scope(heap)?)? {
            return Ok(None);
        }
        let target = self.value(heap, key.node)?;
        let callback = heap
            .table_entry(heap.host_fields(target)?, InputField::OnKey.index() as u32)?
            .1;
        Ok((callback.tag() != ValueTag::Null).then_some((target, callback)))
    }
    pub(in crate::aside) fn input_action(
        &mut self,
        heap: &mut Heap,
        action: &NodeAction,
    ) -> Result<Option<Callback>, AdapterError> {
        let Some(node) = self.scene.node(action.node) else {
            return Ok(None);
        };
        let NodeKind::Input { bindings, .. } = &node.kind else {
            return Ok(None);
        };
        let terminal = matches!(action.phase, ActionPhase::Released | ActionPhase::Cancelled);
        let target = self.value(heap, action.node)?;
        // Terminal events clean old ownership even when visibility or modal scope changed.
        // The UI already routed against accepted modal geometry. Recheck this
        // node's current ancestry/flags without rebuilding the entire scope per axis update.
        if !terminal && !self.mounted_in_scope(heap, action.node, None)? {
            return Ok(None);
        }
        let state = self
            .handles
            .get_mut(&action.node)
            .unwrap()
            .input
            .as_mut()
            .unwrap();
        let payload = heap.host_payload(target, NODE_SLOT)?;
        let (snapshot, index, phase, value, release_retiring) = if *bindings == action.bindings {
            let Some(value) = state.values.get_mut(&action.action) else {
                return Ok(None);
            };
            let source = Source::read(heap, heap.host_fields(target)?)?;
            let replacing = state.source.changed(source);
            if replacing && (!terminal || value.value == 0.) {
                return Ok(None);
            }
            let phase = if replacing {
                ActionPhase::Cancelled
            } else {
                action.phase
            };
            let event_value = if replacing { 0. } else { action.value };
            value.value = event_value;
            (
                heap.sequence_get(payload, CURRENT)?,
                value.index,
                phase,
                event_value,
                false,
            )
        } else {
            let Some(retiring) = &mut state.retiring else {
                return Ok(None);
            };
            if !terminal || retiring.bindings != action.bindings {
                return Ok(None);
            }
            let Some(index) = retiring.held.remove(&action.action) else {
                return Ok(None);
            };
            (
                heap.sequence_get(payload, RETIRING)?,
                index,
                ActionPhase::Cancelled,
                0.,
                retiring.held.is_empty(),
            )
        };
        let row = heap.sequence_get(snapshot, index)?;
        let full = heap.sequence_get(row, 2)?;
        let closure = if full.tag() != ValueTag::Null {
            full
        } else {
            let field = match phase {
                ActionPhase::Pressed => 3,
                ActionPhase::Changed => 4,
                ActionPhase::Released => 5,
                ActionPhase::Cancelled => 6,
                ActionPhase::Repeated => return Ok(None),
            };
            heap.sequence_get(row, field)?
        };
        if release_retiring {
            heap.set_field(payload, RETIRING, Value::NULL)?;
            state.retiring = None;
        }
        Ok((closure.tag() != ValueTag::Null).then_some(Callback {
            target,
            closure,
            phase,
            value,
        }))
    }
}

/// Describe a physical control mapping with a scale and analog dead zone.
/// Put the returned descriptor in a named action's `bindings` array. A plain
/// control name string uses the same mapping with scale 1 and dead_zone 0.
/// `control` names a supported keyboard, pointer or gamepad control. `scale`
/// defaults to 1; `dead_zone` defaults to 0. Invalid controls, non-finite scales
/// and dead zones outside 0 <= dead_zone < 1 are rejected.
/// The descriptor owns no input stream or lifetime; its input node owns dispatch.
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

/// Read the current numeric value of a named action on this input node.
/// Replacing `actions` resets values to neutral until new input arrives.
/// `action` must name an action declared in this node's actions; unknown names
/// are rejected. Reading the value does not consume an event or wait for input.
#[aside_host::native(symbol = "reel::node::input_value", effects = Effects(Effects::HOST), capabilities = ["reel.stage"])]
pub(crate) fn value(
    #[aside(context)] context: &mut CallContext<'_, StageBinding>,
    receiver: Value,
    action: String,
) -> Result<f64, AdapterError> {
    let id = context.host.id(context.vm.heap(), receiver)?;
    // A synchronous read after actions assignment observes the new neutral state.
    let heap = context.vm.heap();
    let source = Source::read(heap, heap.host_fields(receiver)?)?;
    let state = context.host.handles[&id].input.as_ref().unwrap();
    if state.source.changed(source) {
        if action == "back" && source.back.tag() != ValueTag::Null {
            return Ok(0.);
        }
        if source.actions.tag() != ValueTag::Null {
            for index in 0..heap.table_len(source.actions)? {
                if String::from_value(heap, heap.table_entry(source.actions, index)?.0)? == action {
                    return Ok(0.);
                }
            }
        }
        return Err(AdapterError::Input("unknown input action"));
    }
    let value = state
        .values
        .get(action.as_str())
        .ok_or(AdapterError::Input("unknown input action"))?;
    Ok(value.value.into())
}

// Keep the native declarations above as the single source of their English docs.
pub(in crate::aside) fn documentation(symbol: &str) -> Option<&'static str> {
    match symbol {
        "reel::binding" => Some(binding::DOCUMENTATION),
        "reel::node::input_value" => Some(value::DOCUMENTATION),
        _ => None,
    }
}
