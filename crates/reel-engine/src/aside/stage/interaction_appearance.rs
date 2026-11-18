//! Immutable interaction paint projected onto the existing node and input identity.
use super::interaction::{Field, VisualState};
use super::{AdapterError, NodeProperty, overrides, text};
use aside_host::PropertySchema;
use aside_vm::{Heap, HeapError, ObjectKind, Value, ValueTag};

const TAG: i64 = -1112;
const PROPERTIES: &[&str] = &[
    "opacity",
    "invert",
    "color",
    "text_gradient",
    "background",
    "border_width",
    "border_color",
    "corner_radius",
    "rotation",
    "scale_x",
    "scale_y",
    "anchor_x",
    "anchor_y",
    "text_outline",
    "text_outline_color",
    "text_shadow_x",
    "text_shadow_y",
    "text_shadow_color",
];

aside_host::native_documentation! {
    pub(in crate::aside) fn documentation {
        /// Create immutable, shareable interaction appearance for common stage nodes.
        /// Each argument is a table of visual properties, copied and validated once.
        /// Precedence is base, highlight, hover, focus_visible, pressed, disabled, then scope
        /// overrides. Unspecified properties retain the node's ordinary values.
        /// Selection preserves bindings, node identity, focus and ongoing presses.
        ///
        /// Supported properties: opacity, invert, color, text_gradient, background,
        /// border_width, border_color, corner_radius, rotation, scale_x, scale_y,
        /// anchor_x, anchor_y, text_outline, text_outline_color, text_shadow_x,
        /// text_shadow_y and text_shadow_color. Values are literal property values;
        /// bindings belong on the node. Null clears nullable paint properties.
        /// Use `highlight` to share paint between hover and navigation focus.
        ///
        /// ```aside
        /// fn main() {
        ///     let look = appearance(base: {background: "#304060", corner_radius: 6},
        ///         hover: {background: "#506080"}, pressed: {background: "#203040"},
        ///         focus_visible: {border_width: 2, border_color: "#ffffff"},
        ///         disabled: {opacity: 0.4});
        ///     stage.replace(button("Continue", decorated: false, padding: 12,
        ///         appearance: look));
        /// }
        /// ```
        "reel::appearance";
    }
}

pub(in crate::aside) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{NamedType, named, nullable};
    api.refine(
        "reel::appearance",
        named("reel::types::Appearance"),
        &VisualState::NAMES
            .iter()
            .map(|&name| (name, nullable(TypeHint::Table)))
            .collect::<Vec<_>>(),
    );
    api.types.push(NamedType::new("reel::types::Appearance",
        "Immutable common stage paint selected by the node's interaction state. Construct with appearance.", Vec::new()));
}

pub(in crate::aside) fn contracts() -> Vec<aside_host::NativeContract> {
    use aside_vm::bytecode::{BuiltinType, Const, Effects};
    vec![aside_host::NativeContract {
        standard_derived_commit: false,
        read_only_leaf: None,
        symbol: "reel::appearance".into(),
        abi_version: 2,
        parameters: VisualState::NAMES
            .iter()
            .map(|&name| aside_host::NativeParameter {
                name: name.into(),
                ty: BuiltinType::Any,
                default: Some(Const::Null),
            })
            .collect(),
        rest: None,
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE),
        required_capabilities: Vec::new(),
    }]
}

pub(in crate::aside) fn create(
    heap: &mut Heap,
    args: &[Value],
    limit: usize,
) -> Result<Value, AdapterError> {
    let mut payload = [Value::NULL; 1 + VisualState::NAMES.len()];
    payload[0] = Value::int(TAG);
    for (state, &table) in args.iter().enumerate() {
        if table.tag() == ValueTag::Null {
            continue;
        }
        let count = heap.table_len(table)?;
        if count == 0 {
            continue;
        }
        let entries = heap.alloc_array(&[])?;
        for entry in 0..count {
            let (key, value) = heap.table_entry(table, entry)?;
            let name = text::read_string(heap, key, limit)?;
            if !PROPERTIES.contains(&name.as_str()) {
                return Err(AdapterError::Input(
                    "appearance accepts only its documented paint and transform properties",
                ));
            }
            let index = NodeProperty::PROPERTIES
                .iter()
                .position(|property| property.name == name)
                .unwrap();
            // Validating the value itself rejects bindings and mutable nested data.
            NodeProperty::validate(heap, index as u32, value)?;
            heap.array_push(entries, Value::int(index as i64))?;
            heap.array_push(entries, value)?;
        }
        payload[state + 1] = entries;
    }
    let fields = heap.alloc_table()?;
    Ok(heap.alloc_immutable_host(fields, &payload)?)
}

pub(in crate::aside) fn validate(heap: &Heap, value: Value) -> Result<(), HeapError> {
    if value.tag() == ValueTag::Null
        || (heap.info(value)?.kind == ObjectKind::Host
            && heap.host_payload(value, 0)?.as_int() == Some(TAG))
    {
        Ok(())
    } else {
        Err(HeapError::TypeMismatch)
    }
}

/// Sparse prepared entries select into stack storage once per changed projection.
/// No VM allocation, author callback or derived binding is needed for a state change.
pub(super) struct Selection([Option<Value>; NodeProperty::PROPERTIES.len()]);
impl Selection {
    pub(super) fn new(
        heap: &Heap,
        fields: Value,
        offset: usize,
        overrides: Value,
    ) -> Result<Self, HeapError> {
        let mut selection = Self([None; NodeProperty::PROPERTIES.len()]);
        let appearance = overrides::get(
            heap,
            fields,
            offset,
            overrides,
            NodeProperty::Appearance.index(),
        )?;
        if appearance.tag() == ValueTag::Null {
            return Ok(selection);
        }
        validate(heap, appearance)?;
        let interaction = (offset + NodeProperty::PROPERTIES.len()) as u32;
        let table = heap.table_read(fields)?;
        let state = |field: Field| -> Result<bool, HeapError> {
            let index = interaction + field.index() as u32;
            if table.len() <= index {
                return Ok(false);
            }
            Ok(table.value(index)?.as_bool() == Some(true))
        };
        let enabled = overrides::get(
            heap,
            fields,
            offset,
            overrides,
            NodeProperty::Enabled.index(),
        )?
        .as_bool();
        for visual in VisualState::PRIORITY.into_iter().rev() {
            let entries = heap.host_payload(appearance, visual.index() as u32 + 1)?;
            if entries.tag() == ValueTag::Null
                || !visual.active(|field| {
                    if matches!(field, Field::Disabled) && enabled == Some(false) {
                        Ok(true)
                    } else {
                        state(field)
                    }
                })?
            {
                continue;
            }
            for index in (0..heap.sequence_len(entries)?).step_by(2) {
                let property = heap.sequence_get(entries, index)?.as_int().unwrap() as usize;
                selection.0[property] = Some(heap.sequence_get(entries, index + 1)?);
            }
        }
        Ok(selection)
    }
    pub(super) fn get(&self, property: usize) -> Option<Value> {
        self.0[property]
    }
}
