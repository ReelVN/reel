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
        /// Parse rich text markup into an immutable `RichText` value.
        /// Supported tags: `[b]`, `[i]`, `[u]`, `[s]`, `[em]`, `[size=N]`, `[font=family]`,
        /// `[color=#rrggbb]` / `[color=#rrggbbaa]`, `[weight=1..1000]`, `[line_height=N]`,
        /// `[ruby=reading]base[/ruby]`, `[link=id]label[/link]`, `[pause=sec]`,
        /// `[speed=N]` / `[speed=Nx]`, `[next]`, and `[[` for a literal `[`.
        /// Closing nested tags restores the enclosing style. Use `rich_escape` when
        /// interpolating untrusted plain text. Parse errors report their UTF-8 byte offset.
        "reel::rich";
        /// Escape plain text for literal insertion into rich markup.
        "reel::rich_escape";
    }
}
use aside_vm::{Heap, ObjectKind, Value};
use reel_core::text::Text;
const TAG: i64 = -1100;
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
pub(super) fn alloc(heap: &mut Heap, value: &Text, chunk: usize) -> Result<Value, AdapterError> {
    if let Some(markup) = value.markup() {
        let source = text::alloc_string(heap, markup, chunk)?;
        let plain = text::alloc_string(heap, value, chunk)?;
        assemble(heap, source, plain)
    } else {
        text::alloc_string(heap, value, chunk)
    }
}
