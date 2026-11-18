//! Interaction image sources use the normal stage projection and resource resolver.
use super::interaction::{Field, VisualState};
use super::*;

const TAG: i64 = -1113;
const TYPE: &str = "reel::types::ImageSkin";
const VARIANT_START: usize = 3;

aside_host::native_documentation! {
    pub(in crate::aside) fn documentation {
        /// Select an image source and optional atlas region from a stage control's state.
        /// Pass the result as image source. A state accepts an asset path, image bytes,
        /// or `{source, source_rect}`. Use `highlight` for shared hover and navigation
        /// focus artwork. Disabled, pressed, focus_visible and hover overrides take
        /// precedence, in that order. A state can also be a crop tuple on the base
        /// image. Omit source_rect to use the image's crop.
        /// Visible controls prepare their other file-backed states when loading capacity is available.
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
        abi_version: 2,
        parameters: std::iter::once("control")
            .chain(VisualState::NAMES.iter().copied())
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
    let state = TypeHint::Union(vec![
        variant,
        TypeHint::Tuple(vec![aside_host::source::number(); 4]),
    ]);
    parameters.extend(
        VisualState::NAMES[1..]
            .iter()
            .map(|&name| (name, nullable(state.clone()))),
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
    let mut payload = [Value::NULL; VARIANT_START + VisualState::NAMES.len()];
    payload[0] = Value::int(TAG);
    payload[1] = control;
    payload[2] = Value::int(i64::from(stage.handles[&id].interaction_offset));
    payload[VARIANT_START + VisualState::Base.index()] = variant(heap, args[1])?;
    let base_source = heap.sequence_get(payload[VARIANT_START + VisualState::Base.index()], 0)?;
    for (entry, &value) in payload[VARIANT_START + 1..].iter_mut().zip(&args[2..]) {
        if value.tag() != ValueTag::Null {
            *entry = if heap.info(value)?.kind == ObjectKind::Tuple {
                read_source_rect(heap, value)?;
                heap.alloc_array(&[base_source, value])?
            } else {
                variant(heap, value)?
            };
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

/// Keep the immutable paths with the projected skin. Encoded states remain in the
/// VM until selected, so hovering never copies an entire set of encoded images.
pub(super) fn paths(
    heap: &Heap,
    skin: Value,
    read_limit: usize,
) -> Result<Option<std::sync::Arc<[String]>>, AdapterError> {
    let base = heap.sequence_get(
        heap.host_payload(skin, (VARIANT_START + VisualState::Base.index()) as u32)?,
        0,
    )?;
    let mut sources = [base; VisualState::NAMES.len()];
    let mut count = 1;
    for i in 0..VisualState::NAMES.len() {
        let variant = heap.host_payload(skin, (VARIANT_START + i) as u32)?;
        if variant.tag() == ValueTag::Null {
            continue;
        }
        let raw = heap.sequence_get(variant, 0)?;
        if !sources[..count].iter().any(|seen| seen.same_bits(raw)) {
            sources[count] = raw;
            count += 1;
        }
    }
    if count == 1 {
        return Ok(None);
    }
    let mut paths = Vec::new();
    let mut has_encoded = false;
    for &raw in &sources[..count] {
        if heap.info(raw)?.kind == ObjectKind::String {
            let path = read_text(heap, raw, read_limit)?;
            if !paths.contains(&path) {
                paths.push(path);
            }
        } else {
            has_encoded = true;
        }
    }
    Ok((paths.len() > 1 || (has_encoded && !paths.is_empty())).then(|| paths.into()))
}

/// Resolve the raw source and crop before creating or updating the normal image kind.
pub(super) fn select(heap: &Heap, source: Value, rect: Value) -> Result<(Value, Value), HeapError> {
    if !is_skin(heap, source) {
        return Ok((source, rect));
    }
    let host = control(heap, source)?;
    let variant =
        |state: VisualState| heap.host_payload(source, (VARIANT_START + state.index()) as u32);
    let mut selected = variant(VisualState::Base)?;
    if let Ok(fields) = heap.host_fields(host) {
        let offset = heap.host_payload(source, 2)?.as_int().unwrap() as u32;
        let fields = heap.table_read(fields)?;
        let active = |field: Field| -> Result<bool, HeapError> {
            Ok(fields.value(offset + field.index() as u32)?.as_bool() == Some(true))
        };
        for state in VisualState::PRIORITY {
            let candidate = variant(state)?;
            if candidate.tag() != ValueTag::Null && state.active(active)? {
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
