//! Pure scalar math delegates to Rust's floating-point implementation.
use crate::{NativeError, StandardLibrary};
use aside_host::aside_vm::{
    Heap, HeapError, Value,
    bytecode::{BuiltinType, Effects},
};
use aside_host::{CallContext, FromValue, NativeBinding, native};

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
        $(#[doc = $doc])*
        /// Accept int or float, converting integer input to float64. Return float.
        #[native(symbol = $symbol, abi = 1, effects = Effects(0))]
        fn $name(
            #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
            #[aside(doc_type = aside_host::source::number())] value: Number,
        ) -> Result<f64, NativeError> {
            Ok(value.0.$method())
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

/// Return the signed angle of `(x, y)` in radians, preserving the quadrant.
/// Accept int or float inputs, converting integers to float64. Return float.
#[native(symbol = "std::math::atan2", abi = 1, effects = Effects(0))]
fn atan2(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
    #[aside(doc_type = aside_host::source::number())] y: Number,
    #[aside(doc_type = aside_host::source::number())] x: Number,
) -> Result<f64, NativeError> {
    Ok(y.0.atan2(x.0))
}

/// Return the Euclidean length of `(x, y)` with scaled intermediate arithmetic.
/// Accept int or float inputs, converting integers to float64. Return float.
#[native(symbol = "std::math::hypot", abi = 1, effects = Effects(0))]
fn hypot(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
    #[aside(doc_type = aside_host::source::number())] x: Number,
    #[aside(doc_type = aside_host::source::number())] y: Number,
) -> Result<f64, NativeError> {
    Ok(x.0.hypot(y.0))
}

/// Raise `base` to `exponent`, returning a float.
/// Accept int or float inputs, converting integers to float64.
#[native(symbol = "std::math::pow", abi = 1, effects = Effects(0))]
fn pow(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
    #[aside(doc_type = aside_host::source::number())] base: Number,
    #[aside(doc_type = aside_host::source::number())] exponent: Number,
) -> Result<f64, NativeError> {
    Ok(base.0.powf(exponent.0))
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
    ]
}
