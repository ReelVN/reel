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
        /// Parse immutable rich text. Tags control spans, ruby, links and reveal cues.
        /// Font spans select configured families, including blank-line metrics and ruby.
        /// `[weight=400]` accepts finite weights in 1..=1000; `[b]` selects 700.
        /// `[line_height=46]` sets a finite nonnegative absolute logical line height.
        /// Closing nested tags restores the enclosing style.
        /// Use rich_escape for dynamic plain text inside markup. Parse errors report their
        /// UTF-8 byte offset; plain and parsed text use the same dialogue/layout pipeline.
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
        create(heap, source, chunk)
    } else {
        text::alloc_string(heap, value, chunk)
    }
}
