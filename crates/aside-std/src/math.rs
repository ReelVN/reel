//! Pure scalar math delegates to Rust's floating-point implementation.
use crate::{NativeError, StandardLibrary};
use aside_host::aside_vm::{
    Heap, HeapError, Value, ValueTag,
    bytecode::{BuiltinType, Effects, ReadOnlyLeaf},
};
use aside_host::{CallContext, FromValue, NativeBinding, native};
use std::cmp::Ordering;

/// Math accepts the language's two numeric representations at its call boundary.
/// The SDK's ordinary Rust f64 conversion keeps its strict Float contract.
pub(super) struct Number(pub(super) f64);

impl Number {
    pub(super) fn read(value: Value) -> Result<Self, aside_host::Error> {
        value
            .as_int()
            .map(|value| value as f64)
            .or_else(|| value.as_float())
            .map(Self)
            .ok_or_else(|| HeapError::TypeMismatch.into())
    }
}

impl FromValue for Number {
    const TYPE: BuiltinType = BuiltinType::Any;
    fn from_value(_: &Heap, value: Value) -> Result<Self, aside_host::Error> {
        Self::read(value)
    }
}

macro_rules! unary {
    ($($(#[doc = $doc:tt])* $name:ident, $symbol:tt, $method:ident);* $(;)?) => {$ (
        impl Number {
            fn $name(self) -> f64 { self.0.$method() }
        }
        $(#[doc = $doc])*
        /// Accept int or float, converting integer input to float64. Return float.
        #[native(symbol = $symbol, abi = 1, effects = Effects(0),
            read_only_leaf = ReadOnlyLeaf { work: 1, invoke: |_, args| {
                Number::read(args[0]).ok().map(|value| Value::float(value.$name()))
            } })]
        fn $name(
            #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
            #[aside(doc_type = aside_host::source::number())] value: Number,
        ) -> Result<f64, NativeError> {
            Ok(value.$name())
        }
    )*};
}
unary! {
    /// Return the sine of an angle in radians.
    sin, "std::math::sin", sin;
    /// Return the cosine of an angle in radians.
    cos, "std::math::cos", cos;
    /// Return the tangent of an angle in radians.
    tan, "std::math::tan", tan;
    /// Return the inverse sine in radians, or NaN outside [-1, 1].
    asin, "std::math::asin", asin;
    /// Return the inverse cosine in radians, or NaN outside [-1, 1].
    acos, "std::math::acos", acos;
    /// Return the inverse tangent in radians.
    atan, "std::math::atan", atan;
    /// Return the square root, or NaN for a negative value.
    sqrt, "std::math::sqrt", sqrt;
    /// Return e raised to this value.
    exp, "std::math::exp", exp;
    /// Return the natural logarithm.
    ln, "std::math::ln", ln;
    /// Return the base-two logarithm.
    log2, "std::math::log2", log2;
    /// Return the base-ten logarithm.
    log10, "std::math::log10", log10;
    /// Return the absolute value as a float.
    abs, "std::math::abs", abs;
    /// Round down to the nearest integer, returned as a float.
    floor, "std::math::floor", floor;
    /// Round up to the nearest integer, returned as a float.
    ceil, "std::math::ceil", ceil;
    /// Round to the nearest integer, with halfway values rounded away from zero.
    round, "std::math::round", round;
    /// Convert degrees to radians.
    radians, "std::math::radians", to_radians;
    /// Convert radians to degrees.
    degrees, "std::math::degrees", to_degrees;
}

impl Number {
    fn atan2(self, x: Self) -> f64 {
        self.0.atan2(x.0)
    }
    fn hypot(self, y: Self) -> f64 {
        self.0.hypot(y.0)
    }
    fn pow(self, exponent: Self) -> f64 {
        self.0.powf(exponent.0)
    }
}

/// Return the signed angle of `(x, y)` in radians, preserving the quadrant.
/// Accept int or float inputs, converting integers to float64. Return float.
#[native(symbol = "std::math::atan2", abi = 1, effects = Effects(0),
    read_only_leaf = ReadOnlyLeaf { work: 1, invoke: |_, args| {
        Some(Value::float(Number::read(args[0]).ok()?.atan2(Number::read(args[1]).ok()?)))
    } })]
fn atan2(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
    #[aside(doc_type = aside_host::source::number())] y: Number,
    #[aside(doc_type = aside_host::source::number())] x: Number,
) -> Result<f64, NativeError> {
    Ok(y.atan2(x))
}

/// Return the Euclidean length of `(x, y)` with scaled intermediate arithmetic.
/// Accept int or float inputs, converting integers to float64. Return float.
#[native(symbol = "std::math::hypot", abi = 1, effects = Effects(0),
    read_only_leaf = ReadOnlyLeaf { work: 1, invoke: |_, args| {
        Some(Value::float(Number::read(args[0]).ok()?.hypot(Number::read(args[1]).ok()?)))
    } })]
fn hypot(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
    #[aside(doc_type = aside_host::source::number())] x: Number,
    #[aside(doc_type = aside_host::source::number())] y: Number,
) -> Result<f64, NativeError> {
    Ok(x.hypot(y))
}

/// Raise `base` to `exponent`, returning a float.
/// Accept int or float inputs, converting integers to float64.
#[native(symbol = "std::math::pow", abi = 1, effects = Effects(0),
    read_only_leaf = ReadOnlyLeaf { work: 1, invoke: |_, args| {
        Some(Value::float(Number::read(args[0]).ok()?.pow(Number::read(args[1]).ok()?)))
    } })]
fn pow(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
    #[aside(doc_type = aside_host::source::number())] base: Number,
    #[aside(doc_type = aside_host::source::number())] exponent: Number,
) -> Result<f64, NativeError> {
    Ok(base.pow(exponent))
}

/// Return the smaller numeric input, preserving its type and precision.
/// Equal inputs select a. If one input is NaN, select the other.
#[native(symbol = "std::math::min", abi = 1, effects = Effects(0),
    doc_result = aside_host::source::number(),
    read_only_leaf = ReadOnlyLeaf { work: 1, invoke: |_, args| {
        extremum(args[0], args[1], Ordering::Greater).ok()
    } })]
fn min(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
    #[aside(doc_type = aside_host::source::number())] a: Value,
    #[aside(doc_type = aside_host::source::number())] b: Value,
) -> Result<Value, NativeError> {
    extremum(a, b, Ordering::Greater)
}

/// Return the larger numeric input, preserving its type and precision.
/// Equal inputs select a. If one input is NaN, select the other.
#[native(symbol = "std::math::max", abi = 1, effects = Effects(0),
    doc_result = aside_host::source::number(),
    read_only_leaf = ReadOnlyLeaf { work: 1, invoke: |_, args| {
        extremum(args[0], args[1], Ordering::Less).ok()
    } })]
fn max(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
    #[aside(doc_type = aside_host::source::number())] a: Value,
    #[aside(doc_type = aside_host::source::number())] b: Value,
) -> Result<Value, NativeError> {
    extremum(a, b, Ordering::Less)
}

fn extremum(a: Value, b: Value, replace: Ordering) -> Result<Value, NativeError> {
    if !matches!(a.tag(), ValueTag::Int | ValueTag::Float)
        || !matches!(b.tag(), ValueTag::Int | ValueTag::Float)
    {
        return Err(NativeError::Input("min and max require numbers"));
    }
    Ok(
        if a.numeric_cmp(b) == Some(replace) || a.as_float().is_some_and(f64::is_nan) {
            b
        } else {
            a
        },
    )
}

/// Limit a number to an inclusive range, preserving the selected input's type.
/// Reversed or NaN bounds raise Type. A NaN value is returned unchanged.
///
/// ```aside
/// use std::math::clamp;
/// fn main() {
///     if clamp(1.25, 0, 1) != 1 { throw "opacity"; }
/// }
/// ```
#[native(symbol = "std::math::clamp", abi = 1, effects = Effects(0),
    doc_result = aside_host::source::number(),
    read_only_leaf = ReadOnlyLeaf { work: 1, invoke: |_, args| {
        clamp_value(args[0], args[1], args[2]).ok()
    } })]
fn clamp(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
    #[aside(doc_type = aside_host::source::number())] value: Value,
    #[aside(doc_type = aside_host::source::number())] minimum: Value,
    #[aside(doc_type = aside_host::source::number())] maximum: Value,
) -> Result<Value, NativeError> {
    clamp_value(value, minimum, maximum)
}

fn clamp_value(value: Value, minimum: Value, maximum: Value) -> Result<Value, NativeError> {
    if !matches!(value.tag(), ValueTag::Int | ValueTag::Float) {
        return Err(NativeError::Input("clamp requires a number"));
    }
    if !matches!(
        minimum.numeric_cmp(maximum),
        Some(Ordering::Less | Ordering::Equal)
    ) {
        return Err(NativeError::Input("clamp requires ordered numeric bounds"));
    }
    Ok(if value.numeric_cmp(minimum) == Some(Ordering::Less) {
        minimum
    } else if value.numeric_cmp(maximum) == Some(Ordering::Greater) {
        maximum
    } else {
        value
    })
}

/// Convert a number to int, truncating fractional values toward zero.
/// Integer inputs retain all 64 bits. Non-finite or out-of-range floats raise
/// Type at the call site. The conversion allocates no script data.
///
/// ```aside
/// use std::math::to_int;
/// fn main() {
///     let index: int = to_int(1.25 * 20);
///     if index != 25 || to_int(-2.9) != -2 { throw "conversion"; }
/// }
/// ```
#[native(symbol = "std::math::to_int", abi = 1, effects = Effects(0),
    read_only_leaf = ReadOnlyLeaf { work: 1, invoke: |_, args| {
        to_int_value(args[0]).ok().map(Value::int)
    } })]
fn to_int(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
    #[aside(doc_type = aside_host::source::number())] value: Value,
) -> Result<i64, NativeError> {
    to_int_value(value)
}

fn to_int_value(value: Value) -> Result<i64, NativeError> {
    if let Some(integer) = value.as_int() {
        return Ok(integer);
    }
    let value = value
        .as_float()
        .ok_or(NativeError::Input("to_int requires int or float"))?;
    // The upper endpoint is exclusive: i64::MAX rounds up when converted to f64.
    if !(-((1_u64 << 63) as f64)..(1_u64 << 63) as f64).contains(&value) {
        return Err(NativeError::Input(
            "to_int requires a finite value in int64 range",
        ));
    }
    Ok(value as i64)
}

pub(super) fn bindings() -> Vec<NativeBinding<StandardLibrary, NativeError>> {
    vec![
        sin::binding(),
        cos::binding(),
        tan::binding(),
        asin::binding(),
        acos::binding(),
        atan::binding(),
        atan2::binding(),
        sqrt::binding(),
        hypot::binding(),
        pow::binding(),
        exp::binding(),
        ln::binding(),
        log2::binding(),
        log10::binding(),
        abs::binding(),
        floor::binding(),
        ceil::binding(),
        round::binding(),
        radians::binding(),
        degrees::binding(),
        min::binding(),
        max::binding(),
        clamp::binding(),
        to_int::binding(),
    ]
}
