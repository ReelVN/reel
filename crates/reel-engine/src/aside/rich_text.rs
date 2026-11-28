use super::{AdapterError, text};

pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{Member, NamedType, named};
    api.refine(
        "reel::rich",
        named("reel::types::RichText"),
        &[("markup", TypeHint::String)],
    );
    api.refine(
        "reel::rich_escape",
        TypeHint::String,
        &[("text", TypeHint::String)],
    );
    api.types.push(NamedType::new(
        "reel::types::RichText",
        "Immutable parsed text with styled spans and reveal cues.",
        vec![
            Member::field(
                "reel::types::RichText",
                "plain",
                TypeHint::String,
                None,
                "Plain text with markup removed.",
                None,
            ),
            Member::field(
                "reel::types::RichText",
                "markup",
                TypeHint::String,
                None,
                "The original parsed markup.",
                None,
            ),
        ],
    ));
}

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Parse immutable rich text. Style spans use `[b]`, `[i]`, `[u]`, `[s]`,
        /// `[em]`, `[size=24]`, `[font=Family]`, `[line_height=46]`,
        /// `[weight=400]` and `[color=#RRGGBB]`. Color also accepts `#RRGGBBAA`.
        /// Font spans select configured families. Weights range from 1 to 1000,
        /// and `[b]` selects 700. Line height is absolute, in logical units.
        /// Use `[ruby=annotation]base[/ruby]` for ruby and `[link=id]label[/link]`
        /// for links. Close nested spans in reverse order to restore the enclosing style.
        /// `[speed=30]` sets the reveal rate in graphemes per second.
        /// `[speed=2x]` scales the configured rate. `[pause=0.5]` waits in seconds.
        /// `[next]` advances dialogue after reveal when every line in the group uses it.
        /// Write `[[` for a literal open bracket. Use `rich_escape` for dynamic
        /// plain text inside markup. Parse errors report their
        /// UTF-8 byte offset; plain and parsed text use the same dialogue/layout pipeline.
        "reel::rich";
        /// Escape plain text for literal insertion into rich markup.
        "reel::rich_escape";
    }
}
use aside_vm::{Heap, ObjectKind, Value};
use reel_core::text::Text;
const TAG: i64 = -1100;
pub(in crate::aside) const RESOURCE_KIND: &str = "reel.rich_text";
pub(super) fn source(heap: &Heap, value: Value) -> Result<(Value, bool), AdapterError> {
    if heap.info(value)?.kind == ObjectKind::String {
        return Ok((value, false));
    }
    if heap.host_payload_len(value)? != 3 || heap.host_payload(value, 0)?.as_int() != Some(TAG) {
        return Err(AdapterError::Input("expected text or rich text"));
    }
    Ok((heap.host_payload(value, 1)?, true))
}
pub(super) fn decode(source: String, rich: bool) -> Result<Text, AdapterError> {
    if rich {
        Text::parse(source).map_err(AdapterError::Markup)
    } else {
        Ok(source.into())
    }
}
pub(super) fn read(heap: &Heap, value: Value, chunk: usize) -> Result<Text, AdapterError> {
    let (value, rich) = source(heap, value)?;
    decode(text::read_string(heap, value, chunk)?, rich)
}
/// Compare the authored representation, including markup, without parsing or allocation.
pub(super) fn matches(
    heap: &Heap,
    value: Value,
    expected: &Text,
    chunk: usize,
) -> Result<bool, AdapterError> {
    let (value, rich) = source(heap, value)?;
    matches_source(heap, value, rich, expected, chunk)
}

pub(super) fn read_or_reuse(
    heap: &Heap,
    value: Value,
    expected: &Text,
    chunk: usize,
) -> Result<Text, AdapterError> {
    let (value, rich) = source(heap, value)?;
    if matches_source(heap, value, rich, expected, chunk)? {
        Ok(expected.clone())
    } else {
        decode(text::read_string(heap, value, chunk)?, rich)
    }
}

fn matches_source(
    heap: &Heap,
    value: Value,
    rich: bool,
    expected: &Text,
    chunk: usize,
) -> Result<bool, AdapterError> {
    if rich != expected.markup().is_some() {
        return Ok(false);
    }
    text::matches_string(
        heap,
        value,
        expected.markup().map_or(expected.as_ref(), |s| s.as_ref()),
        chunk,
    )
}
pub(super) fn len(heap: &Heap, value: Value) -> Result<usize, AdapterError> {
    let (_, rich) = source(heap, value)?;
    Ok(heap
        .info(if rich {
            heap.host_payload(value, 2)?
        } else {
            value
        })?
        .len as usize)
}
pub(super) fn create(heap: &mut Heap, source: Value, chunk: usize) -> Result<Value, AdapterError> {
    let parsed = decode(text::read_string(heap, source, chunk)?, true)?;
    let plain = text::alloc_string(heap, &parsed, chunk)?;
    assemble(heap, source, plain)
}

/// Build the same immutable host graph from already validated text fields.
pub(in crate::aside) fn assemble(
    heap: &mut Heap,
    source: Value,
    plain: Value,
) -> Result<Value, AdapterError> {
    let fields = heap.alloc_table()?;
    let key = heap.alloc_string("plain")?;
    heap.table_store(fields, 0, key, plain)?;
    let key = heap.alloc_string("markup")?;
    heap.table_store(fields, 1, key, source)?;
    let value = heap.alloc_immutable_host(fields, &[Value::int(TAG), source, plain])?;
    let ty = heap.host_type("reel::types::RichText")?;
    heap.set_host_type(value, ty)?;
    Ok(value)
}

pub(in crate::aside) fn resource_header(
    heap: &Heap,
    value: Value,
) -> Option<super::data::LogicalReference> {
    (heap.host_payload_len(value).ok()? == 3
        && heap.host_payload(value, 0).ok()?.as_int() == Some(TAG))
    .then(|| super::data::LogicalReference {
        kind: RESOURCE_KIND.into(),
        version: 1,
        fields: 2,
        payload: Vec::new(),
    })
}

pub(in crate::aside) fn resource_field(
    heap: &Heap,
    value: Value,
    index: u32,
) -> Result<(std::borrow::Cow<'static, str>, Value), super::data::TypedDataError> {
    let (name, payload) = match index {
        0 => ("plain", 2),
        1 => ("markup", 1),
        _ => {
            return Err(super::data::TypedDataError::Contract(
                "invalid RichText field",
            ));
        }
    };
    Ok((name.into(), heap.host_payload(value, payload)?))
}

pub(in crate::aside) fn allocate_resource(
    heap: &mut Heap,
) -> Result<Value, super::data::TypedDataError> {
    Ok(assemble(heap, Value::NULL, Value::NULL)?)
}

pub(in crate::aside) fn connect_resource(
    heap: &mut Heap,
    value: Value,
    name: &str,
    field: Value,
) -> Result<(), super::data::TypedDataError> {
    let (index, payload) = match name {
        "plain" => (0, 2),
        "markup" => (1, 1),
        _ => {
            return Err(super::data::TypedDataError::Contract(
                "unknown RichText field",
            ));
        }
    };
    let fields = heap.host_fields(value)?;
    let key = heap.table_entry(fields, index)?.0;
    heap.table_store(fields, index, key, field)?;
    heap.set_host_payload(value, payload, field)?;
    Ok(())
}

pub(in crate::aside) fn finish_resource(
    heap: &Heap,
    value: Value,
) -> Result<(), super::data::TypedDataError> {
    for index in 1..=2 {
        if heap.info(heap.host_payload(value, index)?)?.kind != ObjectKind::String {
            return Err(super::data::TypedDataError::Contract(
                "RichText requires string fields",
            ));
        }
    }
    Ok(())
}
pub(super) fn alloc(heap: &mut Heap, value: &Text, chunk: usize) -> Result<Value, AdapterError> {
    if let Some(markup) = value.markup() {
        let source = text::alloc_string(heap, markup, chunk)?;
        let plain = text::alloc_string(heap, value, chunk)?;
        assemble(heap, source, plain)
    } else {
        text::alloc_string(heap, value, chunk)
    }
}
