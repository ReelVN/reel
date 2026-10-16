//! Pure scalar math delegates to Rust's floating-point implementation.
use crate::{NativeError, StandardLibrary};
use aside_host::aside_vm::bytecode::Effects;
use aside_host::{CallContext, NativeBinding, native};

macro_rules! unary {
    ($($name:ident, $symbol:tt, $method:ident);* $(;)?) => {$ (
        #[native(symbol = $symbol, effects = Effects(0))]
        fn $name(
            #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
            value: f64,
        ) -> Result<f64, NativeError> {
            Ok(value.$method())
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

#[native(symbol = "std::math::atan2", effects = Effects(0))]
fn atan2(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
    y: f64,
    x: f64,
) -> Result<f64, NativeError> {
    Ok(y.atan2(x))
}

#[native(symbol = "std::math::hypot", effects = Effects(0))]
fn hypot(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
    x: f64,
    y: f64,
) -> Result<f64, NativeError> {
    Ok(x.hypot(y))
}

#[native(symbol = "std::math::pow", effects = Effects(0))]
fn pow(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
    base: f64,
    exponent: f64,
) -> Result<f64, NativeError> {
    Ok(base.powf(exponent))
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
