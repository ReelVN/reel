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
    ($($name:ident, $symbol:tt, $method:ident);* $(;)?) => {$ (
        #[native(symbol = $symbol, abi = 2, effects = Effects(0))]
        fn $name(
            #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
            value: Number,
        ) -> Result<f64, NativeError> {
            Ok(value.0.$method())
        }
    )*};
}
unary! {
    sin, "std::math::sin", sin;
    cos, "std::math::cos", cos;
    tan, "std::math::tan", tan;
    asin, "std::math::asin", asin;
    acos, "std::math::acos", acos;
    atan, "std::math::atan", atan;
    sqrt, "std::math::sqrt", sqrt;
    exp, "std::math::exp", exp;
    ln, "std::math::ln", ln;
    log2, "std::math::log2", log2;
    log10, "std::math::log10", log10;
    abs, "std::math::abs", abs;
    floor, "std::math::floor", floor;
    ceil, "std::math::ceil", ceil;
    round, "std::math::round", round;
    radians, "std::math::radians", to_radians;
    degrees, "std::math::degrees", to_degrees;
}

#[native(symbol = "std::math::atan2", abi = 2, effects = Effects(0))]
fn atan2(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
    y: Number,
    x: Number,
) -> Result<f64, NativeError> {
    Ok(y.0.atan2(x.0))
}

#[native(symbol = "std::math::hypot", abi = 2, effects = Effects(0))]
fn hypot(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
    x: Number,
    y: Number,
) -> Result<f64, NativeError> {
    Ok(x.0.hypot(y.0))
}

#[native(symbol = "std::math::pow", abi = 2, effects = Effects(0))]
fn pow(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
    base: Number,
    exponent: Number,
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
