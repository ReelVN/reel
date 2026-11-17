//! Interaction image sources use the normal stage projection and resource resolver.
use super::*;

const TAG: i64 = -1113;
const TYPE: &str = "reel::types::ImageSkin";
const STATES: [&str; 5] = ["base", "hover", "focus_visible", "pressed", "disabled"];

aside_host::native_documentation! {
    pub(in crate::aside) fn documentation {
        /// Select an image source and optional atlas region from a stage control's state.
        /// Pass the result as image source. A state accepts an asset path, image bytes,
        /// or `{source, source_rect}`. Omitted states fall back in the order disabled,
        /// pressed, focus_visible, hover, base. Omit source_rect to use the image's crop.
        "reel::image_skin";
    }
}

pub(in crate::aside) fn contract() -> aside_host::NativeContract {
    use aside_host::NativeParameter;
    use aside_vm::bytecode::{BuiltinType, Const, Effects};
    aside_host::NativeContract {
        standard_derived_commit: false,
        read_only_leaf: None,
        symbol: "reel::image_skin".into(),
        abi_version: 1,
        parameters: std::iter::once("control")
            .chain(STATES)
            .enumerate()
            .map(|(index, name)| NativeParameter {
                name: name.into(),
                ty: BuiltinType::Any,
                default: (index > 1).then_some(Const::Null),
            })
            .collect(),
        rest: None,
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE),
        required_capabilities: vec!["reel.stage".into()],
    }
}

pub(in crate::aside) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{NamedType, named, nullable};
    let variant = TypeHint::Union(vec![TypeHint::String, named("Bytes"), TypeHint::Table]);
    let mut parameters = vec![
        ("control", named("reel::types::Node")),
        ("base", variant.clone()),
    ];
    parameters.extend(
        STATES[1..]
            .iter()
            .map(|&name| (name, nullable(variant.clone()))),
    );
    api.refine("reel::image_skin", named(TYPE), &parameters);
    api.types.push(NamedType::new(
        TYPE,
        "An immutable image source selected by a stage control's interaction state.",
        Vec::new(),
    ));
}

pub(super) fn is_skin(heap: &Heap, value: Value) -> bool {
    heap.host_payload(value, 0)
        .is_ok_and(|tag| tag.as_int() == Some(TAG))
}

pub(super) fn binding(
    heap: &Heap,
    shape: snapshot::Shape,
    fields: Value,
) -> Result<Option<HeapHandle>, HeapError> {
    if shape != snapshot::Shape::Image {
        return Ok(None);
    }
    let value = heap.table_entry(fields, 0)?.1;
    is_skin(heap, value).then(|| heap.handle(value)).transpose()
}

fn variant(heap: &mut Heap, value: Value) -> Result<Value, AdapterError> {
    let mut source = value;
    let mut rect = None;
    if heap.info(value)?.kind == ObjectKind::Table {
        source = Value::NULL;
        for index in 0..heap.table_len(value)? {
            let (key, field) = heap.table_entry(value, index)?;
            match text::read_string(heap, key, 11)?.as_str() {
                "source" => source = field,
                "source_rect" => rect = Some(field),
                _ => {
                    return Err(AdapterError::Input(
                        "image skin accepts source and source_rect",
                    ));
                }
            }
        }
    }
    validate_image_property(heap, 0, source)?;
    match rect {
        Some(rect) => {
            read_source_rect(heap, rect)?;
            Ok(heap.alloc_array(&[source, rect])?)
        }
        None => Ok(heap.alloc_array(&[source])?),
    }
}

pub(in crate::aside) fn create(
    stage: &StageBinding,
    heap: &mut Heap,
    args: &[Value],
) -> Result<Value, AdapterError> {
    let control = args[0];
    let id = stage.id(heap, control)?;
    let mut payload = [Value::NULL; 8];
    payload[0] = Value::int(TAG);
    payload[1] = control;
    payload[2] = Value::int(i64::from(stage.handles[&id].interaction_offset));
    payload[3] = variant(heap, args[1])?;
    for (entry, &value) in payload[4..].iter_mut().zip(&args[2..]) {
        if value.tag() != ValueTag::Null {
            *entry = variant(heap, value)?;
        }
    }
    let fields = heap.alloc_table()?;
    let skin = heap.alloc_immutable_host(fields, &payload)?;
    stage.types.mark(heap, skin, TYPE)?;
    Ok(skin)
}

pub(super) fn control(heap: &Heap, skin: Value) -> Result<Value, HeapError> {
    heap.host_payload(skin, 1)
}

/// Resolve the raw source and crop before creating or updating the normal image kind.
pub(super) fn select(heap: &Heap, source: Value, rect: Value) -> Result<(Value, Value), HeapError> {
    if !is_skin(heap, source) {
        return Ok((source, rect));
    }
    let host = control(heap, source)?;
    let mut selected = heap.host_payload(source, 3)?;
    if let Ok(fields) = heap.host_fields(host) {
        let offset = heap.host_payload(source, 2)?.as_int().unwrap() as u32;
        for (state, index) in [(7, 3), (6, 1), (5, 4), (4, 0)] {
            let candidate = heap.host_payload(source, state)?;
            if candidate.tag() != ValueTag::Null
                && heap.table_entry(fields, offset + index)?.1.as_bool() == Some(true)
            {
                selected = candidate;
                break;
            }
        }
    }
    let rect = if heap.sequence_len(selected)? == 2 {
        heap.sequence_get(selected, 1)?
    } else {
        rect
    };
    Ok((heap.sequence_get(selected, 0)?, rect))
}
