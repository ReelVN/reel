//! Immutable text paint shared by node properties and generated editor declarations.
use super::{AdapterError, stage, text};
use aside_vm::{Heap, HeapError, ObjectKind, Value, ValueTag};
use reel_ui::core::{TextGradient, TextGradientSpace};

const TAG: i64 = -1110;

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Create a two-color text gradient. Colors use #rrggbb or #rrggbbaa.
        /// Angle is clockwise in the horizontal paragraph plane: 0 points up and
        /// 180 points down. The paint follows the paragraph's writing_mode.
        /// Space is bounds for the complete paragraph or glyph for each em square.
        /// Assign to text_gradient on text or control captions. Explicit rich-text
        /// colors override it; outlines and shadows keep their own colors.
        ///
        /// ```aside
        /// fn main() {
        ///     stage.replace(text("Morning", text_gradient: linear_gradient("#ffffff", "#999999")));
        /// }
        /// ```
        "reel::linear_gradient";
    }
}

pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{NamedType, named, number};
    api.refine(
        "reel::linear_gradient",
        named("reel::types::LinearGradient"),
        &[
            ("from", TypeHint::String),
            ("to", TypeHint::String),
            ("angle", number()),
            ("space", TypeHint::String),
        ],
    );
    api.types.push(NamedType::new("reel::types::LinearGradient",
        "Immutable two-color text paint. Construct with linear_gradient and assign to text_gradient.", Vec::new()));
}

pub(super) fn contracts() -> Vec<aside_host::NativeContract> {
    use aside_vm::bytecode::{BuiltinType, Const, Effects};
    vec![aside_host::NativeContract {
        standard_derived_commit: false,
        read_only_leaf: None,
        symbol: "reel::linear_gradient".into(),
        abi_version: 1,
        parameters: [
            ("from", BuiltinType::String, None),
            ("to", BuiltinType::String, None),
            ("angle", BuiltinType::Any, Some(Const::Int(180))),
            (
                "space",
                BuiltinType::String,
                Some(Const::Utf8("bounds".into())),
            ),
        ]
        .into_iter()
        .map(|(name, ty, default)| aside_host::NativeParameter {
            name: name.into(),
            ty,
            default,
        })
        .collect(),
        rest: None,
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE),
        required_capabilities: vec![],
    }]
}

pub(super) fn create(heap: &mut Heap, args: &[Value], limit: usize) -> Result<Value, AdapterError> {
    let from = stage::color(heap, args[0])?;
    let to = stage::color(heap, args[1])?;
    let angle = stage::number(args[2])?;
    if !angle.is_finite() {
        return Err(AdapterError::Input("gradient angle must be finite"));
    }
    let space = match text::read_string(heap, args[3], limit)?.as_str() {
        "bounds" => 0,
        "glyph" => 1,
        _ => {
            return Err(AdapterError::Input(
                "gradient space must be bounds or glyph",
            ));
        }
    };
    let fields = heap.alloc_table()?;
    Ok(heap.alloc_immutable_host(
        fields,
        &[
            Value::int(TAG),
            Value::int(u32::from_le_bytes(from) as i64),
            Value::int(u32::from_le_bytes(to) as i64),
            Value::float(angle.rem_euclid(360.)),
            Value::int(space),
        ],
    )?)
}

pub(super) fn validate(heap: &Heap, value: Value) -> Result<(), HeapError> {
    if value.tag() == ValueTag::Null
        || (heap.info(value)?.kind == ObjectKind::Host
            && heap.host_payload(value, 0)?.as_int() == Some(TAG))
    {
        Ok(())
    } else {
        Err(HeapError::TypeMismatch)
    }
}

pub(super) fn project(heap: &Heap, value: Value) -> Result<Option<TextGradient>, HeapError> {
    if value.tag() == ValueTag::Null {
        return Ok(None);
    }
    Ok(Some(TextGradient {
        from: (heap.host_payload(value, 1)?.as_int().unwrap() as u32).to_le_bytes(),
        to: (heap.host_payload(value, 2)?.as_int().unwrap() as u32).to_le_bytes(),
        angle: heap.host_payload(value, 3)?.as_float().unwrap(),
        space: if heap.host_payload(value, 4)?.as_int() == Some(0) {
            TextGradientSpace::Bounds
        } else {
            TextGradientSpace::Glyph
        },
    }))
}
