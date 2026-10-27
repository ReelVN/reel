//! Decimal formatting uses Rust's conversion directly into the budgeted VM heap.
use crate::{NativeError, StandardLibrary};
use aside_host::aside_vm::{Heap, HeapError, ObjectKind, Value, bytecode::Effects};
use aside_host::{CallContext, native};
use std::fmt::{self, Write};

const MAX_PRECISION: i64 = 1024;

#[derive(Clone, Copy)]
enum Mode {
    Fixed,
    Scientific,
}

fn mode(heap: &Heap, value: Value) -> Result<Mode, NativeError> {
    let info = heap.info(value)?;
    if info.kind == ObjectKind::String && info.len <= 10 {
        let mut bytes = [0; 10];
        let mut read = 0;
        while read < info.len as usize {
            let segment = heap.byte_segment(value, read as u32)?;
            bytes[read..read + segment.len()].copy_from_slice(segment);
            read += segment.len();
        }
        match &bytes[..read] {
            b"fixed" => return Ok(Mode::Fixed),
            b"scientific" => return Ok(Mode::Scientific),
            _ => {}
        }
    }
    Err(NativeError::Input(
        "number format mode must be fixed or scientific",
    ))
}

/// Format an int or float as decimal text with round-to-nearest, ties-to-even.
/// `mode: "fixed"` (default) uses `precision` decimal places, from 0 through 1024.
/// `mode: "scientific"` uses 1 through 1024 significant digits, with a lowercase
/// e and an unpadded exponent without a positive sign. The default precision is 2.
/// Trailing zeros are retained. Integers retain all 64 bits; floats are rounded
/// from their actual binary value, so format(2.675, 2) returns "2.67".
/// Negative zero retains its sign. Non-finite values are "NaN", "inf", or "-inf".
/// Invalid modes, precision or input types raise Type at the call site.
/// Output uses the VM's text allocation limits. The precision bound limits one
/// synchronous conversion; this pure operation is also available inside derive.
///
/// ```aside
/// use std::number::format;
/// fn main() {
///     if format(2.675) != "2.67" { throw "binary rounding"; }
///     if format(12.25, precision: 1) != "12.2" { throw "ties to even"; }
///     if format(995, precision: 2, mode: "scientific") != "1.0e3" { throw "carry"; }
/// }
/// ```
#[native(symbol = "std::number::format", abi = 1, effects = Effects(Effects::ALLOCATE), doc_result = aside_host::aside_compiler::semantic::TypeHint::String)]
pub(super) fn format(
    #[aside(context)] context: &mut CallContext<'_, StandardLibrary>,
    #[aside(doc_type = aside_host::source::number())] value: Value,
    #[aside(default = 2)] precision: i64,
    #[aside(name = "mode", default = "fixed", doc_type = aside_host::aside_compiler::semantic::TypeHint::String)]
    mode_value: Value,
) -> Result<Value, NativeError> {
    let selected = mode(context.vm.heap(), mode_value)?;
    let minimum = i64::from(matches!(selected, Mode::Scientific));
    if !(minimum..=MAX_PRECISION).contains(&precision) {
        return Err(NativeError::Input(
            "number format precision is outside the mode's range",
        ));
    }
    let chunk = context.vm.limits().heap.max_mutation_bytes as usize;
    render(
        context.vm.heap_mut(),
        chunk,
        value,
        precision as usize,
        selected,
    )
}

struct Output<'a> {
    heap: &'a mut Heap,
    value: Value,
    chunk: usize,
    error: Option<HeapError>,
}
impl Write for Output<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for bytes in text.as_bytes().chunks(self.chunk) {
            if let Err(error) = self.heap.bytes_append(self.value, bytes) {
                self.error = Some(error);
                return Err(fmt::Error);
            }
        }
        Ok(())
    }
}

fn render(
    heap: &mut Heap,
    chunk: usize,
    value: Value,
    precision: usize,
    mode: Mode,
) -> Result<Value, NativeError> {
    if value.as_int().is_none() && value.as_float().is_none() {
        return Err(NativeError::Input("number format requires int or float"));
    }
    let buffer = heap.alloc_byte_buffer(&[])?;
    let mut output = Output {
        heap,
        value: buffer,
        chunk,
        error: None,
    };
    let result = if let Some(integer) = value.as_int() {
        match mode {
            Mode::Fixed => write!(output, "{integer}").and_then(|()| {
                if precision == 0 {
                    Ok(())
                } else {
                    write!(output, ".{:0<precision$}", "")
                }
            }),
            Mode::Scientific => write!(output, "{integer:.decimals$e}", decimals = precision - 1),
        }
    } else {
        let float = value.as_float().unwrap();
        match mode {
            Mode::Fixed => write!(output, "{float:.precision$}"),
            Mode::Scientific => write!(output, "{float:.decimals$e}", decimals = precision - 1),
        }
    };
    if result.is_err() {
        return Err(output
            .error
            .take()
            .expect("numeric formatting only fails on output")
            .into());
    }
    output.heap.seal_string(buffer)?;
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aside_host::aside_vm::{HeapLimits, SessionEpoch};

    #[test]
    fn decimal_output_obeys_heap_admission_before_allocating_payload() {
        let baseline = Heap::new(SessionEpoch(1), HeapLimits::default())
            .unwrap()
            .stats()
            .charged_bytes;
        let limit = baseline + 1024;
        let mut heap = Heap::new(
            SessionEpoch(1),
            HeapLimits {
                max_bytes: limit,
                max_mutation_bytes: 7,
                ..Default::default()
            },
        )
        .unwrap();
        let result = render(&mut heap, 7, Value::float(f64::MAX), 1024, Mode::Fixed);
        assert!(matches!(
            result,
            Err(NativeError::Host(aside_host::Error::Heap(
                HeapError::ResourceLimit { .. }
            )))
        ));
        assert!(heap.stats().charged_bytes <= limit);
    }
}
